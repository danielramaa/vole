//! The `analyze` screens: the scan as it goes, then the tree, largest first, one folder at a
//! time; and moving what was selected in the home folder to the trash.

use std::cmp::Reverse;
use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
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
use super::tip;
use super::trash::{self, GIO, Gio, Picked, Picker};
use super::tree::{Id, Kind, ROOT, Tree, Unwalked};
use crate::app::ModuleScreen;
use crate::config::Config;
use crate::log::{Log, Outcome};
use crate::term::RunRoot;
use crate::{size, ui};

/// Where the scan starts: the whole filesystem.
const SCANNED: &str = "/";

/// How many cells wide the bar showing an entry's share of its folder is.
const BAR: usize = 10;

pub struct Screen {
    config: Config,
    /// Whether the mounts of other disks are scanned too.
    other_disks: bool,
    state: State,
}

/// What a scan sends back: the tree, and what may be picked in it or why nothing may.
type Scanned = (Tree, Result<Picker, String>);

enum State {
    /// The scan starts on the first tick, so creating the screen touches nothing.
    NotStarted,
    Scanning {
        receiver: Receiver<Result<Scanned, String>>,
        progress: Arc<Progress>,
        home: PathBuf,
    },
    Ready(View),
    /// Asking before anything moves; `in_use` is the extra step for In Use Items.
    Confirm {
        view: View,
        items: Vec<(Id, Picked)>,
        in_use: bool,
        scroll: u16,
    },
    /// Confirmed: the Items start moving on the next tick.
    Confirmed {
        view: View,
        items: Vec<(Id, Picked)>,
    },
    /// The Items are being moved to the trash on a worker thread.
    Trashing {
        view: View,
        items: Vec<(Id, Picked)>,
        receiver: Receiver<Moving>,
        stop: Arc<AtomicBool>,
        now: String,
    },
    Done {
        view: View,
        outcomes: Vec<Outcome>,
        scroll: u16,
    },
    Failed(String),
}

/// Messages from the thread moving Items to the trash.
enum Moving {
    Item(String),
    Finished(Vec<Outcome>),
}

impl Screen {
    pub fn new(config: &Config) -> Self {
        Self {
            config: config.clone(),
            other_disks: false,
            state: State::NotStarted,
        }
    }

    #[cfg(test)]
    fn with_view(view: View) -> Self {
        Self {
            config: Config::default(),
            other_disks: false,
            state: State::Ready(view),
        }
    }

    /// Starts the scan in the background.
    fn start(&self) -> State {
        let Some(home) = dirs::home_dir() else {
            return State::Failed("the home directory could not be found".into());
        };
        // The scan doesn't follow symlinks, so it finds the home folder where it really is.
        let home = fs::canonicalize(&home).unwrap_or(home);
        let (sender, receiver) = mpsc::channel();
        let progress = Arc::new(Progress::default());
        let counting = Arc::clone(&progress);
        let config = self.config.clone();
        let picking = home.clone();
        let other_disks = self.other_disks;
        thread::spawn(move || {
            // Without the mount points, the walk would go into other disks and into /proc.
            let scanned = MountTable::load()
                .map(|mounts| {
                    let tree = scan::scan(Path::new(SCANNED), &mounts, other_disks, &counting);
                    (tree, Picker::load(&picking, &config))
                })
                .map_err(|err| {
                    format!("reading the mount points from /proc/self/mountinfo: {err}")
                });
            // The user may have left the screen already; then nobody is listening.
            let _ = sender.send(scanned);
        });
        State::Scanning {
            receiver,
            progress,
            home,
        }
    }

    /// Fresh path rules and mount points, and the Operation Log: nothing moves without all
    /// three, nor without gio.
    fn prepare(&self, home: &Path) -> Result<(Picker, Log), String> {
        if !Path::new(GIO).exists() {
            return Err("gio not found; install glib2".into());
        }
        let picker = Picker::load(home, &self.config)?;
        let log = Log::open_default("analyze")?;
        Ok((picker, log))
    }

    /// Moves the Items to the trash on a worker thread, so the screen keeps drawing.
    fn start_trashing(view: View, items: Vec<(Id, Picked)>, picker: Picker, mut log: Log) -> State {
        let (sender, receiver) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let picked: Vec<Picked> = items.iter().map(|(_, item)| item.clone()).collect();
        thread::spawn(move || {
            let mut progress = |done: usize, total: usize, label: &str| {
                let _ = sender.send(Moving::Item(format!("{label} ({} of {total})", done + 1)));
            };
            let outcomes = trash::trash_all(
                &picked,
                &picker,
                &Gio,
                &mut log,
                &mut progress,
                &worker_stop,
            );
            let _ = sender.send(Moving::Finished(outcomes));
        });
        State::Trashing {
            view,
            items,
            receiver,
            stop,
            now: String::new(),
        }
    }
}

impl ModuleScreen for Screen {
    /// Moves background work along: the scan, pacman's tips, and moving Items to the trash.
    fn tick(&mut self) {
        let state = std::mem::replace(&mut self.state, State::NotStarted);
        self.state = match state {
            State::NotStarted => self.start(),
            State::Scanning {
                receiver,
                progress,
                home,
            } => match receiver.try_recv() {
                Ok(Ok((tree, picker))) => State::Ready(View::new(tree, home, picker)),
                Ok(Err(err)) => State::Failed(format!("Could not scan: {err}")),
                Err(TryRecvError::Empty) => State::Scanning {
                    receiver,
                    progress,
                    home,
                },
                Err(TryRecvError::Disconnected) => {
                    State::Failed("the scan stopped unexpectedly".into())
                }
            },
            State::Ready(mut view) => {
                view.tick();
                State::Ready(view)
            }
            State::Confirmed { mut view, items } => match self.prepare(&view.home) {
                Ok((picker, log)) => Self::start_trashing(view, items, picker, log),
                Err(err) => {
                    view.flash = Some(format!("Nothing was moved to the trash: {err}"));
                    State::Ready(view)
                }
            },
            State::Trashing {
                mut view,
                items,
                receiver,
                stop,
                mut now,
            } => loop {
                match receiver.try_recv() {
                    Ok(Moving::Item(label)) => now = label,
                    Ok(Moving::Finished(outcomes)) => {
                        view.forget(&items, &outcomes);
                        break State::Done {
                            view,
                            outcomes,
                            scroll: 0,
                        };
                    }
                    Err(TryRecvError::Empty) => {
                        break State::Trashing {
                            view,
                            items,
                            receiver,
                            stop,
                            now,
                        };
                    }
                    Err(TryRecvError::Disconnected) => {
                        break State::Failed(
                            "Moving to the trash stopped unexpectedly; the operation log has what was moved."
                                .into(),
                        );
                    }
                }
            },
            state => state,
        };
    }

    /// Returns whether the key was used.
    fn on_key(&mut self, key: KeyEvent) -> bool {
        let back = matches!(
            key.code,
            KeyCode::Char('n' | 'q' | 'h') | KeyCode::Esc | KeyCode::Left
        );
        if let State::Trashing { stop, .. } = &self.state
            && key.code == KeyCode::Esc
        {
            stop.store(true, Ordering::Relaxed);
            return true;
        }
        let state = std::mem::replace(&mut self.state, State::NotStarted);
        let (state, used) = match state {
            // Scanning again, with or without the other disks.
            State::Ready(_) if key.code == KeyCode::Char('o') => {
                self.other_disks = !self.other_disks;
                (State::NotStarted, true)
            }
            State::Ready(_) if key.code == KeyCode::Char('r') => (State::NotStarted, true),
            State::Ready(mut view) if key.code == KeyCode::Char('t') => {
                let mut items = view.picked();
                if items.is_empty() {
                    view.flash =
                        Some("Nothing selected: space selects what to move to the trash.".into());
                    (State::Ready(view), true)
                } else {
                    view.flash = None;
                    trash::mark_in_use(items.iter_mut().map(|(_, item)| item));
                    let confirm = State::Confirm {
                        view,
                        items,
                        in_use: false,
                        scroll: 0,
                    };
                    (confirm, true)
                }
            }
            State::Ready(mut view) => {
                let used = view.on_key(key);
                (State::Ready(view), used)
            }
            // The In Use step takes another key (Y), so a double-tap or auto-repeat of y can't
            // pass it before it is even drawn.
            State::Confirm {
                view,
                mut items,
                in_use,
                ..
            } if key.code == KeyCode::Char(if in_use { 'Y' } else { 'y' }) => {
                trash::mark_in_use(items.iter_mut().map(|(_, item)| item));
                if !in_use && items.iter().any(|(_, item)| item.in_use) {
                    let confirm = State::Confirm {
                        view,
                        items,
                        in_use: true,
                        scroll: 0,
                    };
                    (confirm, true)
                } else {
                    (State::Confirmed { view, items }, true)
                }
            }
            State::Confirm { view, .. } if back => (State::Ready(view), true),
            State::Confirm {
                view,
                items,
                in_use,
                scroll,
            } => {
                let scroll = ui::scrolled(scroll, key);
                let confirm = State::Confirm {
                    view,
                    items,
                    in_use,
                    scroll,
                };
                (confirm, true)
            }
            state @ (State::Confirmed { .. } | State::Trashing { .. }) => (state, true),
            State::Done { view, .. } if key.code == KeyCode::Enter => (State::Ready(view), true),
            State::Done {
                view,
                outcomes,
                scroll,
            } => {
                let next = ui::scrolled(scroll, key);
                let done = State::Done {
                    view,
                    outcomes,
                    scroll: next,
                };
                (done, next != scroll)
            }
            State::Failed(_) if key.code == KeyCode::Enter => (State::NotStarted, true),
            state => (state, false),
        };
        self.state = state;
        used
    }

    fn hint(&self) -> &'static str {
        match self.state {
            State::Ready(_) => "↑↓/jk move · l/h in/out · space select · t trash… · o other disks",
            State::Confirm { in_use: false, .. } => "↑↓ scroll · y move to the trash · n back",
            State::Confirm { in_use: true, .. } => "↑↓ scroll · Y move anyway · n back",
            State::Trashing { .. } => "esc stop after this Item",
            State::Done { .. } => "↑↓ scroll · enter back to the list",
            State::Failed(_) => "enter scan again",
            State::NotStarted | State::Scanning { .. } | State::Confirmed { .. } => "",
        }
    }

    /// Whether q leaves `analyze` here; while confirming or moving, it doesn't.
    fn can_leave(&self) -> bool {
        match self.state {
            State::NotStarted
            | State::Scanning { .. }
            | State::Ready(_)
            | State::Done { .. }
            | State::Failed(_) => true,
            State::Confirm { .. } | State::Confirmed { .. } | State::Trashing { .. } => false,
        }
    }

    fn needs_terminal(&self) -> bool {
        false
    }

    fn run_root(&mut self, _: &mut RunRoot) -> io::Result<()> {
        Ok(())
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) {
        let title = if self.other_disks {
            " analyze, other disks too "
        } else {
            " analyze "
        };
        let block = Block::bordered().title(title);
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
            State::Confirm {
                items,
                in_use,
                scroll,
                ..
            } => draw_confirm(frame, area, block, items, *in_use, scroll),
            State::Confirmed { .. } => {
                frame.render_widget(Paragraph::new("Moving to the trash…").block(block), area)
            }
            State::Trashing { now, stop, .. } => {
                let text = if stop.load(Ordering::Relaxed) {
                    format!("Stopping after {now}…")
                } else {
                    format!("Moving {now} to the trash…")
                };
                frame.render_widget(Paragraph::new(printable(&text)).block(block), area)
            }
            State::Done {
                outcomes, scroll, ..
            } => ui::draw_outcomes(frame, area, block, outcomes, scroll),
        }
    }
}

fn draw_confirm(
    frame: &mut Frame,
    area: Rect,
    block: Block,
    items: &[(Id, Picked)],
    in_use: bool,
    scroll: &mut u16,
) {
    let bold = Style::new().add_modifier(Modifier::BOLD);
    let width = usize::from(block.inner(area).width);
    let mut lines = Vec::new();
    if in_use {
        let count = items.iter().filter(|(_, item)| item.in_use).count();
        lines.push(Line::styled(
            format!("{count} of these Items are in use by running programs."),
            bold,
        ));
        lines.push(Line::from(
            "Moving them can make those programs misbehave. Press Y (shift+y) to move them anyway.",
        ));
    } else {
        let total = size::format(items.iter().map(|(_, item)| item.size).sum());
        let question = match items.len() {
            1 => format!("Move this Item ({total}) to the trash?"),
            count => format!("Move these {count} Items ({total}) to the trash?"),
        };
        lines.push(Line::styled(question, bold));
        lines.push(Line::from(
            "The trash keeps them, and your file manager can put them back, until it is emptied; vole clean empties it.",
        ));
    }
    lines.push(Line::default());
    for (_, item) in items {
        let mut spans = vec![Span::raw(format!("  {}", printable(&item.label)))];
        if item.in_use {
            spans.push(Span::styled("  in use", bold));
        }
        lines.push(ui::right_aligned(spans, size::format(item.size), width));
    }
    ui::draw_scrolled(frame, area, block, lines, scroll);
}

/// The tree, one folder at a time.
struct View {
    tree: Tree,
    /// The home folder, where it really is.
    home: PathBuf,
    /// What may be picked in the home folder, or why nothing may.
    picker: Result<Picker, String>,
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
    /// The Items selected, in the order they were.
    selected: Vec<Id>,
    /// What the last key did, until the next one: why something can't be selected, say.
    flash: Option<String>,
    /// What pacman said about freeing entries outside the home folder.
    tips: HashMap<Id, String>,
    /// The entry pacman is being asked about, in the background.
    asking: Option<(Id, Receiver<String>)>,
}

impl View {
    fn new(tree: Tree, home: PathBuf, picker: Result<Picker, String>) -> Self {
        let mut view = Self {
            tree,
            home,
            picker,
            dir: ROOT,
            rows: Vec::new(),
            cursor: 0,
            offset: 0,
            page: 1,
            selected: Vec::new(),
            flash: None,
            tips: HashMap::new(),
            asking: None,
        };
        view.open(ROOT, None);
        view
    }

    /// Picks up what pacman said, and asks it about the entry under the cursor when that needs
    /// it: one entry at a time, since pacman takes a moment.
    fn tick(&mut self) {
        if let Some((id, receiver)) = self.asking.take() {
            let tip = match receiver.try_recv() {
                Ok(tip) => tip,
                Err(TryRecvError::Empty) => {
                    self.asking = Some((id, receiver));
                    return;
                }
                Err(TryRecvError::Disconnected) => "pacman couldn't say what it comes from.".into(),
            };
            self.tips.insert(id, tip);
        }
        let Some(id) = self.current().filter(|&id| self.waits_for_pacman(id)) else {
            return;
        };
        let path = self.tree.path(id);
        let folder = self.tree.kind(id) == Kind::Dir;
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            // The user may have left already; then nobody is listening.
            let _ = sender.send(tip::from_pacman(&path, folder));
        });
        self.asking = Some((id, receiver));
    }

    /// Whether `path` is outside the home folder, where vole only shows.
    fn is_outside_home(&self, path: &Path) -> bool {
        !path.starts_with(&self.home) || path == self.home
    }

    /// Whether the tip for `id` is to come from pacman, and hasn't yet.
    fn waits_for_pacman(&self, id: Id) -> bool {
        let path = self.tree.path(id);
        !self.tips.contains_key(&id)
            && !matches!(
                self.tree.unwalked(id),
                Some(Unwalked::Mount(_) | Unwalked::Elsewhere | Unwalked::Through)
            )
            && self.is_outside_home(&path)
            && tip::known(&path, &self.home).is_none()
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

    /// `id`'s path, from `~/` inside the home folder.
    fn label(&self, id: Id) -> String {
        let path = self.tree.path(id);
        match path.strip_prefix(&self.home) {
            Ok(relative) if !relative.as_os_str().is_empty() => {
                format!("~/{}", relative.display())
            }
            _ => path.display().to_string(),
        }
    }

    /// Whether `id` goes to the trash with the selection: it, or a folder holding it, is selected.
    fn is_selected(&self, id: Id) -> bool {
        std::iter::once(id)
            .chain(self.tree.above(id))
            .any(|id| self.selected.contains(&id))
    }

    /// Selects or deselects `id`, when it may be picked; else says why not.
    fn toggle(&mut self, id: Id) {
        if let Some(index) = self.selected.iter().position(|&selected| selected == id) {
            self.selected.remove(index);
            return;
        }
        if let Some(folder) = self
            .tree
            .above(id)
            .find(|folder| self.selected.contains(folder))
        {
            let folder = printable(&self.label(folder));
            self.flash = Some(format!("Selected already, with {folder}."));
            return;
        }
        let path = self.tree.path(id);
        if self.is_outside_home(&path) {
            self.flash = Some(
                "Outside your home folder vole only shows; the tip above says how to free the space."
                    .into(),
            );
            return;
        }
        let checked = match &self.picker {
            Ok(picker) => picker.check(&path),
            Err(err) => {
                self.flash = Some(format!("Nothing can be selected: {err}"));
                return;
            }
        };
        if let Err(why) = checked {
            let label = printable(&self.label(id));
            self.flash = Some(format!("{label} {why}: vole won't move it to the trash."));
            return;
        }
        // What is in it goes with it.
        let tree = &self.tree;
        self.selected
            .retain(|&selected| !tree.above(selected).any(|folder| folder == id));
        self.selected.push(id);
    }

    /// The selected Items, largest first.
    fn picked(&self) -> Vec<(Id, Picked)> {
        let mut items: Vec<(Id, Picked)> = self
            .selected
            .iter()
            .map(|&id| {
                let item = Picked {
                    path: self.tree.path(id),
                    label: self.label(id),
                    size: self.tree.size(id),
                    in_use: false,
                };
                (id, item)
            })
            .collect();
        items.sort_by_key(|(_, item)| Reverse(item.size));
        items
    }

    /// Takes what went to the trash, or was gone already, out of the tree, and clears the
    /// selection. When the folder shown went too, the nearest one above that is left is shown.
    fn forget(&mut self, items: &[(Id, Picked)], outcomes: &[Outcome]) {
        for ((id, _), outcome) in items.iter().zip(outcomes) {
            if outcome.error.is_none() {
                self.tree.remove(*id);
            }
        }
        self.selected.clear();
        let mut dir = self.dir;
        while dir != ROOT && self.tree.is_gone(dir) {
            dir = self.tree.parent(dir).unwrap_or(ROOT);
        }
        let (on, cursor) = (self.current(), self.cursor);
        self.open(dir, on.filter(|&id| !self.tree.is_gone(id)));
        if on.is_some_and(|id| self.tree.is_gone(id)) {
            // Stay about where the cursor was.
            self.cursor = cursor.min(self.rows.len().saturating_sub(1));
        }
    }

    fn on_key(&mut self, key: KeyEvent) -> bool {
        self.flash = None;
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
            KeyCode::Char(' ') => {
                if let Some(id) = self.current() {
                    self.toggle(id);
                }
                return true;
            }
            KeyCode::Char('~') => {
                if let Some(home) = self.tree.find(&self.home) {
                    self.open(home, None);
                }
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
        let [header, list_area, note_area, total_area] = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .areas(inner);
        let bold = Style::new().add_modifier(Modifier::BOLD);
        let dim = Style::new().add_modifier(Modifier::DIM);

        // The folder shown, cut at the start when it is long, and how much it uses.
        let size = self.size_text(self.dir);
        let width = usize::from(header.width);
        let room = width.saturating_sub(Span::raw(size.as_str()).width() + 1);
        let path = printable(&self.tree.path(self.dir).to_string_lossy());
        let path = Span::styled(fit_start(&path, room), bold);
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
        let total = match &self.flash {
            Some(flash) => flash.clone(),
            None if self.selected.is_empty() => String::new(),
            None => {
                let size = self.selected.iter().map(|&id| self.tree.size(id)).sum();
                match self.selected.len() {
                    1 => format!("Selected 1 Item, {}", size::format(size)),
                    count => format!("Selected {count} Items, {}", size::format(size)),
                }
            }
        };
        frame.render_widget(Line::styled(total, bold), total_area);
    }

    /// A row: the entry's name, then its share of the folder (`total`) and its size. Inside
    /// the home folder, a box tells whether it is selected.
    fn line(&self, id: Id, total: u64, width: usize) -> Line<'static> {
        let tree = &self.tree;
        let path = tree.path(id);
        let sealed = self
            .picker
            .as_ref()
            .is_ok_and(|picker| picker.is_sealed(&path));
        let check = if self.is_outside_home(&path) || sealed {
            "    "
        } else if self.is_selected(id) {
            "[x] "
        } else {
            "[ ] "
        };
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
        let mark = if sealed {
            Some("sealed")
        } else {
            self.mark(id)
        };
        let mark = mark.map(|mark| format!("  {mark}"));
        // Long names are cut, so the size always shows.
        let room = width
            .saturating_sub(2 + check.len() + Span::raw(right.as_str()).width() + 1)
            .saturating_sub(mark.as_ref().map_or(0, String::len));
        let mut spans = vec![Span::raw(format!("  {check}{}", fit(&name, room)))];
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
            Some(Unwalked::Through) => Some("not on a disk"),
            Some(Unwalked::Unreadable) => Some("can't read"),
            None if self.tree.linked(id) => Some("hard link"),
            None if self.tree.kind(id) == Kind::Symlink => Some("symlink"),
            None => None,
        }
    }

    /// What to know about `id`, shown under the list while the cursor is on it: how to free it,
    /// outside the home folder.
    fn note(&self, id: Id) -> Option<&str> {
        let path = self.tree.path(id);
        let unwalked = self.tree.unwalked(id);
        if !matches!(
            unwalked,
            Some(Unwalked::Mount(_) | Unwalked::Elsewhere | Unwalked::Through)
        ) && self.is_outside_home(&path)
        {
            return Some(match tip::known(&path, &self.home) {
                Some(tip) => tip,
                None => self
                    .tips
                    .get(&id)
                    .map_or("Asking pacman what it comes from…", String::as_str),
            });
        }
        match unwalked {
            Some(Unwalked::Mount(mount)) => {
                return Some(match mount {
                    Mount::SameDisk => "A mount point.",
                    Mount::OtherDisk => "Another disk is mounted here; o scans other disks too.",
                    Mount::UsbDisk => "A USB disk is mounted here; o scans other disks too.",
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
            Some(Unwalked::Through) => {
                return Some("Not on a disk: only the disks mounted in it are shown.");
            }
            Some(Unwalked::Unreadable) | None => {}
        }
        if let Ok(picker) = &self.picker {
            if picker.is_sealed(&path) {
                return Some("A Sealed Path, or holds one: vole never removes it, even by hand.");
            }
            if picker.is_protected(&path) {
                return Some(
                    "On the Protected List: no module proposes it, but you can select it here.",
                );
            }
        }
        if unwalked == Some(Unwalked::Unreadable) {
            return Some("vole may not read this folder, so what is in it isn't counted.");
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
    use crate::log::MovedTo;
    use crate::testdir::TestDir;

    /// A screen showing `tree`, with `home` as the home folder; nothing can be selected.
    fn screen_of(tree: Tree, home: &str) -> Screen {
        Screen::with_view(View::new(
            tree,
            home.into(),
            Err("no rules in tests".into()),
        ))
    }

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
        screen_of(tree, "/home/me")
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
        let rows = render(&mut screen, 60, 10);
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
            rows[6].contains("Another disk is mounted here"),
            "{rows:#?}"
        );
    }

    /// The note under the list with the cursor on `name`, in a tree of `root` holding `name`.
    fn note_on(root: &str, name: &str, kind: Kind, tip: Option<&str>) -> String {
        let mut tree = Tree::new(root.into(), 0);
        let id = tree.add(ROOT, vec![entry(name, 1, kind)]);
        tree.finish();
        let mut view = View::new(tree, "/home/me".into(), Err("no rules".into()));
        if let Some(tip) = tip {
            view.tips.insert(id, tip.into());
        }
        view.note(id).unwrap_or_default().to_string()
    }

    #[test]
    fn tells_how_to_free_space_outside_home() {
        assert_eq!(
            note_on("/var/cache/pacman", "pkg", Kind::Dir, None),
            "pacman's package cache: vole clean trims it."
        );
        assert_eq!(
            note_on("/usr/lib", "firefox", Kind::Dir, None),
            "Asking pacman what it comes from…"
        );
        assert_eq!(
            note_on(
                "/usr/lib",
                "firefox",
                Kind::Dir,
                Some("From the package firefox.")
            ),
            "From the package firefox."
        );
        assert!(note_on("/home", "me", Kind::Dir, None).starts_with("Your home folder: open it"));
        // Inside it, what is special about an entry.
        assert!(note_on("/home/me", "link", Kind::Symlink, None).starts_with("A symlink"));
        assert_eq!(note_on("/home/me", "notes", Kind::File, None), "");
    }

    #[test]
    fn asks_pacman_only_outside_home_and_once() {
        let mut tree = Tree::new("/".into(), 0);
        let first = tree.add(
            ROOT,
            vec![entry("home", 0, Kind::Dir), entry("opt", 0, Kind::Dir)],
        );
        let me = tree.add(first, vec![entry("me", 0, Kind::Dir)]);
        tree.finish();
        let view = View::new(tree, "/home/me".into(), Err("no rules".into()));
        assert!(view.waits_for_pacman(first + 1));
        // Known without pacman.
        assert!(!view.waits_for_pacman(first) && !view.waits_for_pacman(me));
        let mut view = view;
        view.tips.insert(first + 1, "From the package x.".into());
        assert!(!view.waits_for_pacman(first + 1));
    }

    #[test]
    fn long_names_are_cut_so_sizes_show() {
        let mut tree = Tree::new("/".into(), 0);
        let long = "a-very-long-name-".repeat(10);
        tree.add(ROOT, vec![entry(&long, 2048, Kind::File)]);
        tree.finish();
        let mut screen = screen_of(tree, "/home/me");
        let rows = render(&mut screen, 50, 6);
        assert!(rows[2].contains("a-very-lo…"), "{rows:#?}");
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
        let mut screen = screen_of(tree, "/home/me");
        render(&mut screen, 50, 12);
        press(&mut screen, KeyCode::PageDown);
        assert_eq!(rows(&screen).1, "file007");
        press(&mut screen, KeyCode::Char('G'));
        let rows = render(&mut screen, 50, 12);
        assert!(
            rows[8].contains("> ") && rows[8].contains("file099"),
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
            config: Config::default(),
            other_disks: false,
            state: State::Scanning {
                receiver,
                progress,
                home: "/home/me".into(),
            },
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

    /// A home folder holding `Videos/old.mkv`, `Videos/new.mkv`, `.ssh/id` and `notes`, and a
    /// screen showing it, scanned from the home folder itself.
    fn home_screen() -> (TestDir, Screen) {
        let home = TestDir::new();
        home.file("Videos/old.mkv", 30_000);
        home.file("Videos/new.mkv", 20_000);
        home.file(".ssh/id", 100);
        home.file("notes", 10);
        let tree = scan::scan(
            home.path(),
            &MountTable::default(),
            false,
            &Progress::default(),
        );
        let picker = Picker::load(home.path(), &Config::default());
        let view = View::new(tree, home.path().into(), picker);
        (home, Screen::with_view(view))
    }

    /// Moves the cursor to the row named `name`.
    fn go_to(screen: &mut Screen, name: &str) {
        press(screen, KeyCode::Home);
        while rows(screen).1 != name {
            assert!(press(screen, KeyCode::Down));
            assert!(rows(screen).0.iter().any(|row| row == name), "no {name}");
        }
    }

    fn selected(screen: &Screen) -> Vec<String> {
        let view = view(screen);
        view.selected.iter().map(|&id| view.label(id)).collect()
    }

    fn flash(screen: &Screen) -> String {
        view(screen).flash.clone().unwrap_or_default()
    }

    #[test]
    fn selects_in_the_home_folder_but_never_sealed_paths() {
        let (_home, mut screen) = home_screen();
        go_to(&mut screen, "notes");
        press(&mut screen, KeyCode::Char(' '));
        assert_eq!(selected(&screen), ["~/notes"]);
        go_to(&mut screen, ".ssh");
        press(&mut screen, KeyCode::Char(' '));
        assert_eq!(
            flash(&screen),
            "~/.ssh is a sealed path: vole won't move it to the trash."
        );
        assert_eq!(selected(&screen), ["~/notes"]);
        // The next key clears the message.
        press(&mut screen, KeyCode::Char('k'));
        assert_eq!(flash(&screen), "");
        go_to(&mut screen, "notes");
        press(&mut screen, KeyCode::Char(' '));
        assert!(selected(&screen).is_empty());
    }

    #[test]
    fn a_folder_takes_what_is_in_it_along() {
        let (_home, mut screen) = home_screen();
        go_to(&mut screen, "Videos");
        press(&mut screen, KeyCode::Char('l'));
        press(&mut screen, KeyCode::Char(' '));
        assert_eq!(selected(&screen), ["~/Videos/old.mkv"]);
        press(&mut screen, KeyCode::Char('h'));
        press(&mut screen, KeyCode::Char(' '));
        assert_eq!(selected(&screen), ["~/Videos"]);
        press(&mut screen, KeyCode::Char('l'));
        press(&mut screen, KeyCode::Char(' '));
        assert_eq!(flash(&screen), "Selected already, with ~/Videos.");
        press(&mut screen, KeyCode::Char('k'));
        let rows = render(&mut screen, 70, 8).join("\n");
        assert!(rows.contains("[x] new.mkv"), "{rows}");
        assert!(rows.contains("Selected 1 Item"), "{rows}");
    }

    #[test]
    fn nothing_outside_home_is_selected() {
        let mut screen = screen();
        press(&mut screen, KeyCode::Char(' '));
        assert!(view(&screen).selected.is_empty());
        assert!(flash(&screen).starts_with("Outside your home folder vole only shows"));
        let rows = render(&mut screen, 60, 9);
        assert!(!rows.join("").contains("[ ]"), "{rows:#?}");
    }

    #[test]
    fn rows_in_home_have_boxes_and_sealed_ones_say_so() {
        let (_home, mut screen) = home_screen();
        let rows = render(&mut screen, 70, 9).join("\n");
        assert!(rows.contains("[ ] Videos/"), "{rows}");
        assert!(rows.contains("    .ssh/  sealed"), "{rows}");
        go_to(&mut screen, ".ssh");
        let rows = render(&mut screen, 90, 9).join("\n");
        assert!(rows.contains("A Sealed Path, or holds one"), "{rows}");
    }

    #[test]
    fn t_asks_before_moving_anything() {
        let (_home, mut screen) = home_screen();
        press(&mut screen, KeyCode::Char('t'));
        assert!(flash(&screen).starts_with("Nothing selected"));
        go_to(&mut screen, "Videos");
        press(&mut screen, KeyCode::Char(' '));
        press(&mut screen, KeyCode::Char('t'));
        assert!(!screen.can_leave());
        let rows = render(&mut screen, 100, 10).join("\n");
        assert!(rows.contains("Move this Item ("), "{rows}");
        assert!(rows.contains(") to the trash?"), "{rows}");
        assert!(rows.contains("~/Videos"), "{rows}");
        // Back keeps the selection.
        assert!(press(&mut screen, KeyCode::Char('n')));
        assert_eq!(selected(&screen), ["~/Videos"]);
        press(&mut screen, KeyCode::Char('t'));
        press(&mut screen, KeyCode::Char('y'));
        assert!(matches!(screen.state, State::Confirmed { .. }));
        assert!(press(&mut screen, KeyCode::Char('q')) && !screen.can_leave());
    }

    #[test]
    fn in_use_items_need_a_second_confirmation() {
        let (_home, mut screen) = home_screen();
        go_to(&mut screen, "notes");
        press(&mut screen, KeyCode::Char(' '));
        press(&mut screen, KeyCode::Char('t'));
        if let State::Confirm { items, .. } = &mut screen.state {
            items[0].1.in_use = true;
        }
        press(&mut screen, KeyCode::Char('y'));
        let rows = render(&mut screen, 100, 10).join("\n");
        assert!(rows.contains("1 of these Items are in use"), "{rows}");
        // A second y (a double-tap, auto-repeat) doesn't pass the In Use step.
        press(&mut screen, KeyCode::Char('y'));
        assert!(matches!(screen.state, State::Confirm { in_use: true, .. }));
        press(&mut screen, KeyCode::Char('Y'));
        assert!(matches!(screen.state, State::Confirmed { .. }));
    }

    /// The screen as the thread moving `names` to the trash runs.
    fn trashing(names: &[&str]) -> (TestDir, Screen, mpsc::Sender<Moving>) {
        let (home, mut screen) = home_screen();
        for name in names {
            go_to(&mut screen, name);
            press(&mut screen, KeyCode::Char(' '));
        }
        press(&mut screen, KeyCode::Char('t'));
        let State::Confirm { view, items, .. } =
            std::mem::replace(&mut screen.state, State::NotStarted)
        else {
            panic!("not confirming");
        };
        let (sender, receiver) = mpsc::channel();
        screen.state = State::Trashing {
            view,
            items,
            receiver,
            stop: Arc::new(AtomicBool::new(false)),
            now: String::new(),
        };
        (home, screen, sender)
    }

    fn outcome(label: &str, error: Option<&str>) -> Outcome {
        Outcome {
            label: label.into(),
            size: 1,
            error: error.map(String::from),
            log_error: None,
            moved: Some(MovedTo::Trash),
        }
    }

    #[test]
    fn shows_what_moves_and_stops_on_esc() {
        let (_home, mut screen, sender) = trashing(&["notes"]);
        sender
            .send(Moving::Item("~/notes (1 of 1)".into()))
            .unwrap();
        screen.tick();
        let rows = render(&mut screen, 60, 5).join("");
        assert!(
            rows.contains("Moving ~/notes (1 of 1) to the trash…"),
            "{rows}"
        );
        assert_eq!(screen.hint(), "esc stop after this Item");
        assert!(press(&mut screen, KeyCode::Esc));
        assert!(!screen.can_leave());
        assert!(
            render(&mut screen, 60, 5)
                .join("")
                .contains("Stopping after ~/notes")
        );
    }

    #[test]
    fn what_went_leaves_the_list() {
        let (_home, mut screen, sender) = trashing(&["Videos", "notes"]);
        let (before, videos) = match &screen.state {
            State::Trashing { view, items, .. } => {
                (view.tree.size(ROOT), view.tree.size(items[0].0))
            }
            _ => panic!("not trashing"),
        };
        // Largest first: Videos, then notes, which failed.
        sender
            .send(Moving::Finished(vec![
                outcome("~/Videos", None),
                outcome("~/notes", Some("Permission denied")),
            ]))
            .unwrap();
        screen.tick();
        let rows = render(&mut screen, 70, 10).join("\n");
        assert!(rows.contains("✓ ~/Videos → trash"), "{rows}");
        assert!(rows.contains("✗ ~/notes"), "{rows}");
        assert_eq!(screen.hint(), "↑↓ scroll · enter back to the list");
        assert!(press(&mut screen, KeyCode::Enter));
        let (names, _) = super::tests::rows(&screen);
        assert!(!names.contains(&"Videos".to_string()), "{names:?}");
        assert!(names.contains(&"notes".to_string()));
        assert!(view(&screen).selected.is_empty());
        assert_eq!(view(&screen).tree.size(ROOT), before - videos);
    }

    #[test]
    fn the_folder_shown_goes_up_when_it_went() {
        let (_home, mut screen, sender) = trashing(&["Videos"]);
        if let State::Trashing { view, .. } = &mut screen.state {
            let videos = view.current().unwrap();
            view.open(videos, None);
        }
        sender
            .send(Moving::Finished(vec![outcome("~/Videos", None)]))
            .unwrap();
        screen.tick();
        press(&mut screen, KeyCode::Enter);
        assert_eq!(view(&screen).dir, ROOT);
    }

    #[test]
    fn o_scans_again_with_other_disks_and_r_just_again() {
        let mut screen = screen();
        assert!(press(&mut screen, KeyCode::Char('o')));
        assert!(screen.other_disks && matches!(screen.state, State::NotStarted));
        let rows = render(&mut screen, 60, 6);
        assert!(rows[0].contains(" analyze, other disks too "), "{rows:#?}");
        let mut again = super::tests::screen();
        assert!(press(&mut again, KeyCode::Char('r')));
        assert!(!again.other_disks && matches!(again.state, State::NotStarted));
    }

    #[test]
    fn tilde_goes_to_the_home_folder() {
        let mut tree = Tree::new("/".into(), 0);
        let first = tree.add(
            ROOT,
            vec![entry("usr", 10, Kind::Dir), entry("home", 0, Kind::Dir)],
        );
        let me = tree.add(first + 1, vec![entry("me", 0, Kind::Dir)]);
        tree.add(me, vec![entry("notes", 1, Kind::File)]);
        tree.finish();
        let mut screen = screen_of(tree, "/home/me");
        assert!(press(&mut screen, KeyCode::Char('~')));
        assert_eq!(view(&screen).dir, me);
        assert_eq!(rows(&screen).0, ["notes"]);
    }
}
