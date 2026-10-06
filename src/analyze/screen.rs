//! The `analyze` screens: the scan as it goes, then the tree, largest first, one folder at a
//! time.

use std::io;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;

use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, List, ListState, Paragraph};

use super::disks::{Mount, MountTable};
use super::scan::{self, Progress};
use super::tree::{Id, Kind, ROOT, Tree, Unwalked};
use crate::app::ModuleScreen;
use crate::term::RunRoot;
use crate::{size, ui};

/// Where the scan starts: the whole filesystem.
const SCANNED: &str = "/";

/// How many cells wide the bar showing an entry's share of its folder is.
const BAR: usize = 10;

pub struct Screen {
    state: State,
}

enum State {
    /// The scan starts on the first tick, so creating the screen touches nothing.
    NotStarted,
    Scanning {
        receiver: Receiver<Result<Tree, String>>,
        progress: Arc<Progress>,
    },
    Ready(View),
    Failed(String),
}

impl Default for Screen {
    fn default() -> Self {
        Self::new()
    }
}

impl Screen {
    pub fn new() -> Self {
        Self {
            state: State::NotStarted,
        }
    }

    #[cfg(test)]
    fn with_tree(tree: Tree) -> Self {
        Self {
            state: State::Ready(View::new(tree)),
        }
    }

    /// Starts the scan in the background.
    fn start() -> State {
        let (sender, receiver) = mpsc::channel();
        let progress = Arc::new(Progress::default());
        let counting = Arc::clone(&progress);
        thread::spawn(move || {
            // Without the mount points, the walk would go into other disks and into /proc.
            let tree = MountTable::load()
                .map(|mounts| scan::scan(Path::new(SCANNED), &mounts, &counting))
                .map_err(|err| {
                    format!("reading the mount points from /proc/self/mountinfo: {err}")
                });
            // The user may have left the screen already; then nobody is listening.
            let _ = sender.send(tree);
        });
        State::Scanning { receiver, progress }
    }
}

impl ModuleScreen for Screen {
    /// Starts the scan, and picks up the tree once it is done.
    fn tick(&mut self) {
        let state = std::mem::replace(&mut self.state, State::NotStarted);
        self.state = match state {
            State::NotStarted => Self::start(),
            State::Scanning { receiver, progress } => match receiver.try_recv() {
                Ok(Ok(tree)) => State::Ready(View::new(tree)),
                Ok(Err(err)) => State::Failed(format!("Could not scan: {err}")),
                Err(TryRecvError::Empty) => State::Scanning { receiver, progress },
                Err(TryRecvError::Disconnected) => {
                    State::Failed("the scan stopped unexpectedly".into())
                }
            },
            state => state,
        };
    }

    /// Returns whether the key was used.
    fn on_key(&mut self, key: KeyEvent) -> bool {
        match &mut self.state {
            State::Ready(view) => view.on_key(key),
            State::Failed(_) if key.code == KeyCode::Enter => {
                self.state = State::NotStarted;
                true
            }
            State::NotStarted | State::Scanning { .. } | State::Failed(_) => false,
        }
    }

    fn hint(&self) -> &'static str {
        match self.state {
            State::Ready(_) => "↑↓/jk move · l open · h up",
            State::Failed(_) => "enter scan again",
            State::NotStarted | State::Scanning { .. } => "",
        }
    }

    /// q always leaves `analyze`: nothing it does has to finish first.
    fn can_leave(&self) -> bool {
        true
    }

    fn needs_terminal(&self) -> bool {
        false
    }

    fn run_root(&mut self, _: &mut RunRoot) -> io::Result<()> {
        Ok(())
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) {
        let block = Block::bordered().title(" analyze ");
        match &mut self.state {
            State::NotStarted => {
                frame.render_widget(Paragraph::new("Scanning…").block(block), area)
            }
            State::Scanning { progress, .. } => {
                let text = format!(
                    "Scanning… {} files and folders, {} so far",
                    count(progress.found.load(Ordering::Relaxed)),
                    size::format(progress.bytes.load(Ordering::Relaxed)),
                );
                frame.render_widget(Paragraph::new(text).block(block), area)
            }
            State::Failed(err) => {
                frame.render_widget(Paragraph::new(err.as_str()).block(block), area)
            }
            State::Ready(view) => view.draw(frame, area, block),
        }
    }
}

/// The tree, one folder at a time.
struct View {
    tree: Tree,
    /// The folder shown.
    dir: Id,
    /// What is in it, largest first.
    rows: Vec<Id>,
    /// The row the cursor is on.
    cursor: usize,
    /// The first row on screen.
    offset: usize,
    /// How many rows were on screen last time, for page up and down.
    page: usize,
}

impl View {
    fn new(tree: Tree) -> Self {
        let mut view = Self {
            tree,
            dir: ROOT,
            rows: Vec::new(),
            cursor: 0,
            offset: 0,
            page: 1,
        };
        view.open(ROOT, None);
        view
    }

    /// Shows the folder `dir`, with the cursor on `on` when it is there.
    fn open(&mut self, dir: Id, on: Option<Id>) {
        self.rows = self.tree.largest_first(dir);
        self.dir = dir;
        self.cursor = on
            .and_then(|on| self.rows.iter().position(|&id| id == on))
            .unwrap_or(0);
        self.offset = 0;
    }

    fn current(&self) -> Option<Id> {
        self.rows.get(self.cursor).copied()
    }

    fn on_key(&mut self, key: KeyEvent) -> bool {
        let last = self.rows.len().saturating_sub(1);
        let to = match key.code {
            KeyCode::Down | KeyCode::Char('j') => self.cursor + 1,
            KeyCode::Up | KeyCode::Char('k') => self.cursor.saturating_sub(1),
            KeyCode::PageDown => self.cursor + self.page,
            KeyCode::PageUp => self.cursor.saturating_sub(self.page),
            KeyCode::Home | KeyCode::Char('g') => 0,
            KeyCode::End | KeyCode::Char('G') => last,
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => {
                if let Some(id) = self.current().filter(|&id| self.tree.can_open(id)) {
                    self.open(id, None);
                }
                return true;
            }
            KeyCode::Left | KeyCode::Char('h') | KeyCode::Backspace => {
                // At the top, h and ← leave analyze.
                let Some(parent) = self.tree.parent(self.dir) else {
                    return false;
                };
                self.open(parent, Some(self.dir));
                return true;
            }
            _ => return false,
        };
        self.cursor = to.min(last);
        true
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect, block: Block) {
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let [header, list_area, note_area] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .areas(inner);
        let dim = Style::new().add_modifier(Modifier::DIM);

        // The folder shown, cut at the start when it is long, and how much it uses.
        let size = self.size_text(self.dir);
        let width = usize::from(header.width);
        let room = width.saturating_sub(Span::raw(size.as_str()).width() + 1);
        let path = printable(&self.tree.path(self.dir).to_string_lossy());
        let path = Span::styled(
            fit_start(&path, room),
            Style::new().add_modifier(Modifier::BOLD),
        );
        frame.render_widget(ui::right_aligned(vec![path], size, width), header);

        if self.rows.is_empty() {
            frame.render_widget(Line::styled("  Nothing in this folder.", dim), list_area);
        } else {
            // Only the rows on screen are made: a folder may hold many thousands.
            let height = usize::from(list_area.height).max(1);
            self.page = height;
            self.offset = self
                .offset
                .min(self.cursor)
                .max((self.cursor + 1).saturating_sub(height))
                .min(self.rows.len().saturating_sub(height));
            let end = (self.offset + height).min(self.rows.len());
            // Room for the highlight symbol.
            let width = usize::from(list_area.width).saturating_sub(2);
            let total = self.tree.size(self.dir);
            let lines: Vec<Line> = self.rows[self.offset..end]
                .iter()
                .map(|&id| self.line(id, total, width))
                .collect();
            let list = List::new(lines)
                .highlight_style(Style::new().add_modifier(Modifier::REVERSED))
                .highlight_symbol("> ");
            let mut state = ListState::default().with_selected(Some(self.cursor - self.offset));
            frame.render_stateful_widget(list, list_area, &mut state);
        }

        if let Some(note) = self.current().and_then(|id| self.note(id)) {
            frame.render_widget(Line::styled(note, dim), note_area);
        }
    }

    /// A row: the entry's name, then its share of the folder (`total`) and its size.
    fn line(&self, id: Id, total: u64, width: usize) -> Line<'static> {
        let tree = &self.tree;
        let size = tree.size(id);
        let right = format!(
            "{} {:>4}  {:>12}",
            bar(size, total),
            format!("{}%", share(size, total, 100)),
            self.size_text(id)
        );
        let mut name = printable(&tree.name(id).to_string_lossy());
        if tree.kind(id) == Kind::Dir {
            name.push('/');
        }
        let mark = self.mark(id).map(|mark| format!("  {mark}"));
        // Long names are cut, so the size always shows.
        let room = width
            .saturating_sub(2 + Span::raw(right.as_str()).width() + 1)
            .saturating_sub(mark.as_ref().map_or(0, String::len));
        let mut spans = vec![Span::raw(format!("  {}", fit(&name, room)))];
        if let Some(mark) = mark {
            spans.push(Span::styled(mark, Style::new().add_modifier(Modifier::DIM)));
        }
        ui::right_aligned(spans, right, width)
    }

    /// `id`'s size, as "at least" when something in it couldn't be read.
    fn size_text(&self, id: Id) -> String {
        let size = size::format(self.tree.size(id));
        if self.tree.incomplete(id) {
            format!("≥ {size}")
        } else {
            size
        }
    }

    /// A word or two after the name, saying what is special about `id`.
    fn mark(&self, id: Id) -> Option<&'static str> {
        match self.tree.unwalked(id) {
            Some(Unwalked::Mount(mount)) => Some(match mount {
                Mount::SameDisk => "mount point",
                Mount::OtherDisk => "other disk",
                Mount::UsbDisk => "USB disk",
                Mount::Network => "network share",
                Mount::Automount => "not mounted",
                Mount::NotADisk => "not on a disk",
            }),
            Some(Unwalked::Elsewhere) => Some("counted elsewhere"),
            Some(Unwalked::Unreadable) => Some("can't read"),
            None if self.tree.linked(id) => Some("hard link"),
            None if self.tree.kind(id) == Kind::Symlink => Some("symlink"),
            None => None,
        }
    }

    /// What to know about `id`, shown under the list while the cursor is on it.
    fn note(&self, id: Id) -> Option<&'static str> {
        match self.tree.unwalked(id) {
            Some(Unwalked::Mount(mount)) => {
                return Some(match mount {
                    Mount::SameDisk => "A mount point.",
                    Mount::OtherDisk => "Another disk is mounted here; it isn't scanned.",
                    Mount::UsbDisk => "A USB disk is mounted here; it isn't scanned.",
                    Mount::Network => {
                        "A network share is mounted here; vole doesn't look into network shares."
                    }
                    Mount::Automount => "Mounted when first used, and not mounted now.",
                    Mount::NotADisk => {
                        "Not on a disk: kept in memory, made up by the kernel, or a mounted image file."
                    }
                });
            }
            Some(Unwalked::Elsewhere) => {
                return Some(
                    "The same folder as one at another path (a bind mount), where it is counted.",
                );
            }
            Some(Unwalked::Unreadable) => {
                return Some("vole may not read this folder, so what is in it isn't counted.");
            }
            None => {}
        }
        if self.tree.incomplete(id) {
            return Some("Some folders in it can't be read, so it uses at least this much.");
        }
        if self.tree.linked(id) {
            return Some("A hard link: the space is counted at another of its names.");
        }
        (self.tree.kind(id) == Kind::Symlink)
            .then_some("A symlink: what it points to is counted where it is.")
    }
}

/// `part`'s share of `whole`, out of `scale`.
fn share(part: u64, whole: u64, scale: u64) -> u64 {
    if whole == 0 {
        return 0;
    }
    let share = u128::from(part) * u128::from(scale) / u128::from(whole);
    u64::try_from(share).unwrap_or(u64::MAX).min(scale)
}

/// `size`'s share of `total` as a bar [`BAR`] cells wide, drawn in eighths of a cell.
fn bar(size: u64, total: u64) -> String {
    const EIGHTHS: [&str; 8] = ["", "▏", "▎", "▍", "▌", "▋", "▊", "▉"];
    let eighths = share(size, total, BAR as u64 * 8) as usize;
    let mut bar = "█".repeat(eighths / 8);
    bar.push_str(EIGHTHS[eighths % 8]);
    let cells = eighths.div_ceil(8);
    bar.push_str(&" ".repeat(BAR - cells));
    bar
}

/// `name` with control characters, which would break the row, shown as `?`.
fn printable(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_control() { '?' } else { c })
        .collect()
}

/// `text` cut to `width` columns, ending with … where it was cut.
fn fit(text: &str, width: usize) -> String {
    if Span::raw(text).width() <= width {
        return text.to_string();
    }
    let mut fitted = String::new();
    // The … takes one column.
    let mut used = 1;
    for c in text.chars() {
        let columns = Span::raw(c.to_string()).width();
        if used + columns > width {
            break;
        }
        used += columns;
        fitted.push(c);
    }
    fitted.push('…');
    fitted
}

/// `text` cut at the start to `width` columns, so its end stays.
fn fit_start(text: &str, width: usize) -> String {
    let reversed: String = text.chars().rev().collect();
    fit(&reversed, width).chars().rev().collect()
}

/// `n` with its thousands apart, e.g. `1,234,567`.
fn count(n: u64) -> String {
    let digits = n.to_string();
    let mut text = String::new();
    for (i, digit) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            text.push(',');
        }
        text.push(digit);
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyze::tree::Entry;

    fn entry(name: &str, size: u64, kind: Kind) -> Entry {
        Entry {
            name: name.into(),
            size,
            kind,
            unwalked: None,
            linked: false,
        }
    }

    /// `/` holding `usr` (`lib` and `share`), `swapfile`, `home` (`notes`), a symlink and a
    /// mount point.
    fn screen() -> Screen {
        let mut tree = Tree::new("/".into(), 0);
        let mut mnt = entry("mnt", 0, Kind::Dir);
        mnt.unwalked = Some(Unwalked::Mount(Mount::OtherDisk));
        let first = tree.add(
            ROOT,
            vec![
                entry("usr", 0, Kind::Dir),
                entry("home", 0, Kind::Dir),
                entry("swapfile", 300, Kind::File),
                mnt,
                entry("bin", 0, Kind::Symlink),
            ],
        );
        tree.add(
            first,
            vec![
                entry("share", 100, Kind::File),
                entry("lib", 500, Kind::File),
            ],
        );
        tree.add(first + 1, vec![entry("notes", 10, Kind::File)]);
        tree.finish();
        Screen::with_tree(tree)
    }

    fn press(screen: &mut Screen, code: KeyCode) -> bool {
        screen.on_key(KeyEvent::from(code))
    }

    fn view(screen: &Screen) -> &View {
        match &screen.state {
            State::Ready(view) => view,
            _ => panic!("no tree"),
        }
    }

    /// The names of the rows, and the one the cursor is on.
    fn rows(screen: &Screen) -> (Vec<String>, String) {
        let view = view(screen);
        let name = |id: Id| view.tree.name(id).to_string_lossy().into_owned();
        let names = view.rows.iter().map(|&id| name(id)).collect();
        (names, view.current().map(name).unwrap_or_default())
    }

    fn render(screen: &mut Screen, width: u16, height: u16) -> Vec<String> {
        let backend = ratatui::backend::TestBackend::new(width, height);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| screen.draw(frame, frame.area()))
            .unwrap();
        let buffer = terminal.backend().buffer();
        (0..height)
            .map(|y| (0..width).map(|x| buffer[(x, y)].symbol()).collect())
            .collect()
    }

    #[test]
    fn shows_the_largest_first() {
        let screen = screen();
        let (names, on) = rows(&screen);
        assert_eq!(names, ["usr", "swapfile", "home", "bin", "mnt"]);
        assert_eq!(on, "usr");
    }

    #[test]
    fn opens_folders_and_comes_back_to_them() {
        let mut screen = screen();
        press(&mut screen, KeyCode::Char('j'));
        press(&mut screen, KeyCode::Char('j'));
        assert!(press(&mut screen, KeyCode::Char('l')));
        assert_eq!(rows(&screen), (vec!["notes".into()], "notes".into()));
        assert!(press(&mut screen, KeyCode::Char('h')));
        assert_eq!(rows(&screen).1, "home");
        press(&mut screen, KeyCode::Home);
        assert!(press(&mut screen, KeyCode::Enter));
        assert_eq!(rows(&screen).0, ["lib", "share"]);
        assert!(press(&mut screen, KeyCode::Left));
        assert_eq!(rows(&screen).1, "usr");
    }

    #[test]
    fn files_and_mount_points_do_not_open() {
        let mut screen = screen();
        press(&mut screen, KeyCode::Char('j'));
        assert!(press(&mut screen, KeyCode::Char('l')));
        assert_eq!(rows(&screen).1, "swapfile");
        press(&mut screen, KeyCode::End);
        assert!(press(&mut screen, KeyCode::Enter));
        assert_eq!(rows(&screen).1, "mnt");
    }

    #[test]
    fn leaving_keys_leave_from_the_top() {
        let mut screen = screen();
        assert!(!press(&mut screen, KeyCode::Char('h')));
        assert!(!press(&mut screen, KeyCode::Char('q')));
        assert!(!press(&mut screen, KeyCode::Esc));
        assert!(screen.can_leave());
        press(&mut screen, KeyCode::Enter);
        // Deeper, h goes up instead.
        assert!(press(&mut screen, KeyCode::Char('h')));
        assert!(!press(&mut screen, KeyCode::Char('q')));
    }

    #[test]
    fn rows_show_their_share_of_the_folder() {
        let mut screen = screen();
        let rows = render(&mut screen, 60, 9);
        assert!(
            rows[1].contains("/") && rows[1].contains("910 B"),
            "{rows:#?}"
        );
        assert!(rows[2].contains("> "), "{rows:#?}");
        // usr is 600 of 910 bytes: 65%, six and a half cells of the bar.
        assert!(
            rows[2].contains("usr/") && rows[2].contains("██████▌     65%"),
            "{rows:#?}"
        );
        assert!(rows[2].trim_end_matches('│').trim_end().ends_with("600 B"));
        assert!(rows[5].contains("bin  symlink"), "{rows:#?}");
        assert!(rows[6].contains("mnt/  other disk"), "{rows:#?}");
        assert_eq!(view(&screen).page, 5);
    }

    #[test]
    fn says_what_is_special_about_the_entry_under_the_cursor() {
        let mut screen = screen();
        press(&mut screen, KeyCode::End);
        let rows = render(&mut screen, 70, 9);
        assert!(
            rows[7].contains("Another disk is mounted here"),
            "{rows:#?}"
        );
    }

    #[test]
    fn long_names_are_cut_so_sizes_show() {
        let mut tree = Tree::new("/".into(), 0);
        let long = "a-very-long-name-".repeat(10);
        tree.add(ROOT, vec![entry(&long, 2048, Kind::File)]);
        tree.finish();
        let mut screen = Screen::with_tree(tree);
        let rows = render(&mut screen, 50, 6);
        assert!(rows[2].contains("a-very-long-n…"), "{rows:#?}");
        assert!(
            rows[2]
                .trim_end_matches('│')
                .trim_end()
                .ends_with("2.0 KiB")
        );
    }

    #[test]
    fn scrolls_long_folders() {
        let mut tree = Tree::new("/".into(), 0);
        let entries = (0..100)
            .map(|i| entry(&format!("file{i:03}"), 1000 - i, Kind::File))
            .collect();
        tree.add(ROOT, entries);
        tree.finish();
        let mut screen = Screen::with_tree(tree);
        render(&mut screen, 50, 12);
        press(&mut screen, KeyCode::PageDown);
        assert_eq!(rows(&screen).1, "file008");
        press(&mut screen, KeyCode::Char('G'));
        let rows = render(&mut screen, 50, 12);
        assert!(
            rows[9].contains("> ") && rows[9].contains("file099"),
            "{rows:#?}"
        );
        press(&mut screen, KeyCode::PageUp);
        press(&mut screen, KeyCode::Char('g'));
        assert_eq!(super::tests::rows(&screen).1, "file000");
    }

    #[test]
    fn shows_how_far_the_scan_has_got() {
        let (_sender, receiver) = mpsc::channel();
        let progress = Arc::new(Progress::default());
        progress.found.store(1_234_567, Ordering::Relaxed);
        progress.bytes.store(2048, Ordering::Relaxed);
        let mut screen = Screen {
            state: State::Scanning { receiver, progress },
        };
        let rows = render(&mut screen, 70, 5).join("\n");
        assert!(
            rows.contains("Scanning… 1,234,567 files and folders, 2.0 KiB so far"),
            "{rows}"
        );
    }

    #[test]
    fn cuts_text_by_columns() {
        assert_eq!(fit("abcdef", 6), "abcdef");
        assert_eq!(fit("abcdef", 4), "abc…");
        assert_eq!(fit("日本語", 4), "日…");
        assert_eq!(fit_start("/home/me/Videos", 8), "…/Videos");
        assert_eq!(printable("a\nb"), "a?b");
        assert_eq!(count(999), "999");
        assert_eq!(count(1000), "1,000");
        assert_eq!(bar(1, 1), "█".repeat(BAR));
        assert_eq!(bar(0, 0), " ".repeat(BAR));
    }
}
