//! The `orphans` screens: looking for Orphaned Packages, the Plan with selection, confirmation,
//! and the result.

use std::io;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;

use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, List, ListItem, ListState, Paragraph};

use super::build_plan;
use super::pacman::{ConfigFile, FileState};
use super::plan::{Package, Plan};
use super::remove;
use crate::app::ModuleScreen;
use crate::log::{Log, Outcome};
use crate::term::RunRoot;
use crate::{size, ui};

pub struct Screen {
    state: State,
}

enum State {
    /// The scan starts on the first tick, so creating the screen touches nothing.
    NotStarted,
    Scanning(Receiver<Result<Plan, String>>),
    Ready(PlanView),
    /// Asking before anything is removed.
    Confirm {
        view: PlanView,
        scroll: u16,
    },
    /// Confirmed: waiting for the App to lend the terminal to `sudo pacman -Rns`.
    RootPending(Plan),
    Done {
        outcomes: Vec<Outcome>,
        scroll: u16,
    },
    Failed(String),
}

impl Screen {
    pub fn new() -> Self {
        Self {
            state: State::NotStarted,
        }
    }

    /// Starts building the Plan in the background.
    fn start() -> Receiver<Result<Plan, String>> {
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let plan = build_plan().map_err(|err| format!("{err:#}"));
            // The user may have left the screen already; then nobody is listening.
            let _ = sender.send(plan);
        });
        receiver
    }

    #[cfg(test)]
    pub fn with_plan(plan: Plan) -> Self {
        Self {
            state: State::Ready(PlanView::new(plan)),
        }
    }

    #[cfg(test)]
    pub fn has_plan(&self) -> bool {
        matches!(self.state, State::Ready(_))
    }
}

impl ModuleScreen for Screen {
    /// Moves background work along: starts the scan and picks up its result.
    fn tick(&mut self) {
        let state = std::mem::replace(&mut self.state, State::NotStarted);
        self.state = match state {
            State::NotStarted => State::Scanning(Self::start()),
            State::Scanning(receiver) => match receiver.try_recv() {
                Ok(Ok(plan)) => State::Ready(PlanView::new(plan)),
                Ok(Err(err)) => {
                    State::Failed(format!("Could not look for orphaned packages: {err}"))
                }
                Err(TryRecvError::Empty) => State::Scanning(receiver),
                Err(TryRecvError::Disconnected) => {
                    State::Failed("the scan stopped unexpectedly".into())
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
        let state = std::mem::replace(&mut self.state, State::NotStarted);
        let (state, used) = match state {
            State::Ready(view) if key.code == KeyCode::Enter => {
                if view.plan.selected().next().is_some() {
                    (State::Confirm { view, scroll: 0 }, true)
                } else {
                    (State::Ready(view), true)
                }
            }
            State::Ready(mut view) => {
                let used = view.on_key(key);
                (State::Ready(view), used)
            }
            State::Confirm { view, .. } if key.code == KeyCode::Char('y') => {
                (State::RootPending(view.plan), true)
            }
            State::Confirm { view, .. } if back => (State::Ready(view), true),
            State::Confirm { view, scroll } => (
                State::Confirm {
                    view,
                    scroll: ui::scrolled(scroll, key),
                },
                true,
            ),
            state @ State::RootPending(_) => (state, true),
            State::Done { .. } | State::Failed(_) if key.code == KeyCode::Enter => {
                (State::NotStarted, true)
            }
            State::Done { outcomes, scroll } => {
                let next = ui::scrolled(scroll, key);
                (
                    State::Done {
                        outcomes,
                        scroll: next,
                    },
                    next != scroll,
                )
            }
            state => (state, false),
        };
        self.state = state;
        used
    }

    fn hint(&self) -> &'static str {
        match self.state {
            State::Ready(_) => "↑↓/jk move · space select · a select all · enter remove…",
            State::Confirm { .. } => "↑↓ scroll · y remove · n back",
            State::Done { .. } => "↑↓ scroll · enter scan again",
            State::Failed(_) => "enter scan again",
            State::NotStarted | State::Scanning(_) | State::RootPending(_) => "",
        }
    }

    /// Whether q leaves `orphans` here; while confirming or removing, it doesn't.
    fn can_leave(&self) -> bool {
        match self.state {
            State::NotStarted
            | State::Scanning(_)
            | State::Ready(_)
            | State::Done { .. }
            | State::Failed(_) => true,
            State::Confirm { .. } | State::RootPending(_) => false,
        }
    }

    /// Whether `sudo pacman -Rns` is waiting for [`Screen::run_root`].
    fn needs_terminal(&self) -> bool {
        matches!(self.state, State::RootPending(_))
    }

    /// Removes the confirmed packages; `run_root` runs `sudo pacman -Rns` with the TUI paused.
    /// Fails when the TUI couldn't be brought back afterwards; the result is logged either way.
    fn run_root(&mut self, run_root: &mut RunRoot) -> io::Result<()> {
        let state = std::mem::replace(&mut self.state, State::NotStarted);
        let State::RootPending(plan) = state else {
            self.state = state;
            return Ok(());
        };
        // Nothing is removed without the log to write it to.
        let mut log = match Log::open_default("orphans") {
            Ok(log) => log,
            Err(err) => {
                self.state = State::Failed(format!("Nothing was removed: {err}"));
                return Ok(());
            }
        };
        let (outcomes, terminal) = remove::remove(&plan, &mut log, run_root, &remove::System);
        self.state = State::Done {
            outcomes,
            scroll: 0,
        };
        terminal
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) {
        let block = Block::bordered().title(" orphans ");
        match &mut self.state {
            State::NotStarted | State::Scanning(_) => frame.render_widget(
                Paragraph::new("Looking for orphaned packages…").block(block),
                area,
            ),
            State::Failed(err) => {
                frame.render_widget(Paragraph::new(err.as_str()).block(block), area)
            }
            State::Ready(view) => view.draw(frame, area, block),
            State::Confirm { view, scroll } => draw_confirm(frame, area, block, &view.plan, scroll),
            State::RootPending(_) => {
                frame.render_widget(Paragraph::new("Removing…").block(block), area)
            }
            State::Done { outcomes, scroll } => {
                ui::draw_outcomes(frame, area, block, outcomes, scroll)
            }
        }
    }
}

fn draw_confirm(frame: &mut Frame, area: Rect, block: Block, plan: &Plan, scroll: &mut u16) {
    let selected: Vec<&Package> = plan.selected().collect();
    let width = usize::from(block.inner(area).width);
    let mut lines = vec![
        Line::styled(
            match selected.len() {
                1 => format!(
                    "Remove this package, freeing about {}?",
                    size::format(plan.selected_size())
                ),
                count => format!(
                    "Remove these {count} packages, freeing about {}?",
                    size::format(plan.selected_size())
                ),
            },
            Style::new().add_modifier(Modifier::BOLD),
        ),
        Line::from("vole pauses so sudo can ask for your password; pacman then asks once more."),
    ];
    if selected
        .iter()
        .any(|package| !package.config_files.is_empty())
    {
        lines.push(Line::from(
            "Their config files go too, even changed ones: -n keeps no .pacsave copies.",
        ));
    }
    lines.push(Line::default());
    for package in &selected {
        let name = vec![Span::raw(format!("  {}", package.name))];
        lines.push(ui::right_aligned(name, size::format(package.size), width));
        lines.extend(config_file_lines(&package.config_files, 4));
    }
    ui::draw_scrolled(frame, area, block, lines, scroll);
}

/// The Plan as a list: a heading row, then one row per package.
struct PlanView {
    plan: Plan,
    list: ListState,
}

impl PlanView {
    fn new(plan: Plan) -> Self {
        Self {
            plan,
            list: ListState::default().with_selected(Some(0)),
        }
    }

    /// The package on the current row; `None` on the heading.
    fn current(&self) -> Option<usize> {
        self.list.selected()?.checked_sub(1)
    }

    fn on_key(&mut self, key: KeyEvent) -> bool {
        let last = self.plan.packages.len();
        let index = self.list.selected().unwrap_or(0).min(last);
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => self.list.select(Some((index + 1).min(last))),
            KeyCode::Up | KeyCode::Char('k') => self.list.select(Some(index.saturating_sub(1))),
            KeyCode::Char(' ') => match self.current() {
                Some(package) => self.plan.toggle(package),
                None => self.plan.toggle_all(),
            },
            KeyCode::Char('a') => self.plan.toggle_all(),
            _ => return false,
        }
        true
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect, block: Block) {
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let [list_area, total_area] =
            Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(inner);

        // Room for the highlight symbol.
        let width = usize::from(list_area.width).saturating_sub(2);
        let height = usize::from(list_area.height).max(1);
        let mut items = vec![ListItem::new(self.heading(width))];
        items.extend((0..self.plan.packages.len()).map(|i| self.item(i, width, height)));
        let list = List::new(items)
            .highlight_style(Style::new().add_modifier(Modifier::REVERSED))
            .highlight_symbol("> ");
        frame.render_stateful_widget(list, list_area, &mut self.list);

        let total = format!(
            " Selected {} of {}",
            size::format(self.plan.selected_size()),
            size::format(self.plan.size()),
        );
        frame.render_widget(
            Line::styled(total, Style::new().add_modifier(Modifier::BOLD)),
            total_area,
        );
    }

    fn heading(&self, width: usize) -> Line<'static> {
        let mut spans = vec![Span::styled(
            "Orphaned packages",
            Style::new().add_modifier(Modifier::BOLD),
        )];
        if self.plan.packages.is_empty() {
            spans.push(Span::styled(
                "  none found",
                Style::new().add_modifier(Modifier::DIM),
            ));
            return Line::from(spans);
        }
        ui::right_aligned(spans, size::format(self.plan.size()), width)
    }

    /// A package's row, with its config files below, in at most `height` lines: ratatui draws
    /// nothing at all when the highlighted item doesn't fit. The confirmation lists every file.
    fn item(&self, index: usize, width: usize, height: usize) -> ListItem<'static> {
        let dim = Style::new().add_modifier(Modifier::DIM);
        let package = &self.plan.packages[index];
        let check = if package.selected { "[x]" } else { "[ ]" };
        let indent = if package.nested { "      " } else { "  " };
        let mut spans = vec![Span::raw(format!("{indent}{check} {}", package.name))];
        if !package.also_needs.is_empty() {
            let verb = if package.also_needs.len() == 1 {
                "is"
            } else {
                "are"
            };
            spans.push(Span::styled(
                format!(
                    "  once {} {verb} removed too",
                    package.also_needs.join(", ")
                ),
                dim,
            ));
        }
        if !package.optional_for.is_empty() {
            spans.push(Span::styled(
                format!("  still optional for {}", package.optional_for.join(", ")),
                dim,
            ));
        }
        let mut lines = vec![ui::right_aligned(spans, size::format(package.size), width)];
        // Under the name.
        let pad = indent.len() + 6;
        let mut files = config_file_lines(&package.config_files, pad);
        if 1 + files.len() > height {
            // Room for the name and the line saying how many more.
            let shown = height.saturating_sub(2);
            let more = files.len() - shown;
            files.truncate(shown);
            if height >= 2 {
                files.push(Line::styled(
                    format!(
                        "{}… and {more} more, listed when confirming",
                        " ".repeat(pad)
                    ),
                    dim,
                ));
            }
        }
        lines.extend(files);
        ListItem::new(lines)
    }
}

/// A package's config files, one per line, `indent` columns in, marking the changed ones.
fn config_file_lines(files: &[ConfigFile], indent: usize) -> Vec<Line<'static>> {
    let dim = Style::new().add_modifier(Modifier::DIM);
    files
        .iter()
        .map(|file| {
            let mut spans = vec![Span::styled(
                format!("{}{}", " ".repeat(indent), file.path),
                dim,
            )];
            match file.state {
                FileState::Modified => spans.push(Span::styled(
                    "  modified",
                    Style::new().add_modifier(Modifier::BOLD),
                )),
                FileState::Unreadable => spans.push(Span::styled("  unreadable", dim)),
                FileState::Unmodified => {}
            }
            Line::from(spans)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orphans::plan::Package;

    fn screen() -> Screen {
        Screen::with_plan(Plan {
            packages: vec![Package::new("a", 10), Package::new("b", 20)],
        })
    }

    fn selected_size(screen: &Screen) -> u64 {
        match &screen.state {
            State::Ready(view) => view.plan.selected_size(),
            _ => panic!("no plan"),
        }
    }

    fn press(screen: &mut Screen, code: KeyCode) -> bool {
        screen.on_key(KeyEvent::from(code))
    }

    #[test]
    fn space_selects_a_package_or_all_on_the_heading() {
        let mut screen = screen();
        press(&mut screen, KeyCode::Char('j'));
        press(&mut screen, KeyCode::Char('j'));
        press(&mut screen, KeyCode::Char(' '));
        assert_eq!(selected_size(&screen), 20);
        press(&mut screen, KeyCode::Char('k'));
        press(&mut screen, KeyCode::Char('k'));
        press(&mut screen, KeyCode::Char(' '));
        assert_eq!(selected_size(&screen), 30);
        press(&mut screen, KeyCode::Char('a'));
        assert_eq!(selected_size(&screen), 0);
    }

    #[test]
    fn movement_stays_in_bounds() {
        let mut screen = screen();
        press(&mut screen, KeyCode::Up);
        for _ in 0..10 {
            press(&mut screen, KeyCode::Down);
        }
        press(&mut screen, KeyCode::Char(' '));
        assert_eq!(selected_size(&screen), 20);
    }

    /// The screen's rows as text.
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
    fn shows_config_files_under_their_package() {
        let mut cups = Package::new("cups", 10);
        cups.config_files = vec![ConfigFile {
            path: "/etc/cups/cupsd.conf".into(),
            state: FileState::Modified,
        }];
        let mut screen = Screen::with_plan(Plan {
            packages: vec![cups, Package::new("zstd", 1)],
        });
        let rows = render(&mut screen, 60, 8);
        assert!(rows[2].contains("  [ ] cups"), "{rows:#?}");
        assert!(rows[3].contains("        /etc/cups/cupsd.conf  modified"));
        assert!(rows[4].contains("  [ ] zstd"));
    }

    #[test]
    fn a_package_with_more_config_files_than_rows_still_shows() {
        let mut cups = Package::new("cups", 10);
        cups.config_files = (0..30)
            .map(|i| ConfigFile {
                path: format!("/etc/cups/{i}.conf"),
                state: FileState::Unmodified,
            })
            .collect();
        let mut screen = Screen::with_plan(Plan {
            packages: vec![Package::new("a", 1), cups],
        });
        press(&mut screen, KeyCode::Char('j'));
        press(&mut screen, KeyCode::Char('j'));
        // Borders and the total leave 9 rows for the list: the name, 7 files and the rest.
        let rows = render(&mut screen, 60, 12).join("\n");
        assert!(rows.contains("[ ] cups"), "{rows}");
        assert!(rows.contains("/etc/cups/6.conf"), "{rows}");
        assert!(
            rows.contains("… and 23 more, listed when confirming"),
            "{rows}"
        );
    }

    #[test]
    fn enter_asks_for_confirmation_only_with_a_selection() {
        let mut screen = screen();
        assert!(press(&mut screen, KeyCode::Enter));
        assert!(screen.has_plan());
        press(&mut screen, KeyCode::Char('a'));
        press(&mut screen, KeyCode::Enter);
        assert!(!screen.has_plan() && !screen.can_leave());
        // Backing out keeps the selection, and q goes back instead of leaving.
        assert!(press(&mut screen, KeyCode::Char('q')));
        assert!(screen.can_leave());
        assert_eq!(selected_size(&screen), 30);
        press(&mut screen, KeyCode::Enter);
        press(&mut screen, KeyCode::Char('y'));
        assert!(screen.needs_terminal());
    }

    #[test]
    fn keys_do_nothing_while_removing() {
        let mut screen = screen();
        press(&mut screen, KeyCode::Char('a'));
        press(&mut screen, KeyCode::Enter);
        press(&mut screen, KeyCode::Char('y'));
        assert!(press(&mut screen, KeyCode::Char('q')));
        assert!(press(&mut screen, KeyCode::Esc));
        assert!(screen.needs_terminal() && !screen.can_leave());
    }

    #[test]
    fn confirmation_lists_the_packages_and_their_config_files() {
        let mut cups = Package::new("cups", 10);
        cups.config_files = vec![ConfigFile {
            path: "/etc/cups/cupsd.conf".into(),
            state: FileState::Modified,
        }];
        cups.selected = true;
        let mut screen = Screen::with_plan(Plan {
            packages: vec![cups, Package::new("zstd", 1)],
        });
        press(&mut screen, KeyCode::Enter);
        let rows = render(&mut screen, 90, 10).join("\n");
        assert!(
            rows.contains("Remove this package, freeing about 10 B?"),
            "{rows}"
        );
        assert!(rows.contains("-n keeps no .pacsave copies"));
        assert!(rows.contains("│  cups "));
        assert!(rows.contains("│    /etc/cups/cupsd.conf  modified"));
        assert!(!rows.contains("zstd"));
    }

    #[test]
    fn leaving_keys_are_not_used() {
        let mut screen = screen();
        assert!(!press(&mut screen, KeyCode::Char('q')));
        assert!(!press(&mut screen, KeyCode::Esc));
        assert!(screen.can_leave());
    }
}
