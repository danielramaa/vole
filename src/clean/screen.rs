//! The `clean` screens: scanning, the Plan with selection, confirmation, and the result.

use std::io;
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

use super::plan::{Item, Plan};
use super::remove;
use super::scan::{ALLOWED_ROOTS, Mounts};
use super::{build_plan, refresh_in_use};
use crate::config::Config;
use crate::log::{Log, Outcome};
use crate::safety::Rules;
use crate::term::RunRoot;
use crate::{size, ui};

pub struct Screen {
    config: Config,
    state: State,
}

enum State {
    /// The scan starts on the first tick, so creating the screen touches nothing.
    NotStarted,
    Scanning(Receiver<Result<Plan, String>>),
    Ready(PlanView),
    /// Asking before anything is removed; `in_use` is the extra step for In Use Items.
    Confirm {
        view: PlanView,
        in_use: bool,
        scroll: u16,
    },
    /// Confirmed: the removal starts on the next tick.
    Removing(PlanView),
    /// The home Items are being removed on a worker thread.
    RemovingHome {
        plan: Plan,
        receiver: Receiver<Progress>,
        stop: Arc<AtomicBool>,
        now: String,
    },
    /// The home Items are done; the root commands wait for the App to lend the terminal.
    RootPending {
        plan: Plan,
        outcomes: Vec<Outcome>,
        log: Log,
    },
    Done {
        outcomes: Vec<Outcome>,
        scroll: u16,
    },
    Failed(String),
}

/// Messages from the removal thread.
enum Progress {
    Item(String),
    Finished(Vec<Outcome>, Log),
}

impl Screen {
    pub fn new(config: &Config) -> Self {
        Self {
            config: config.clone(),
            state: State::NotStarted,
        }
    }

    /// Starts building the Plan in the background.
    fn start(config: Config) -> Receiver<Result<Plan, String>> {
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let plan = match dirs::home_dir() {
                Some(home) => build_plan(&home, &config).map_err(|err| format!("{err:#}")),
                None => Err("the home directory could not be found".into()),
            };
            // The user may have left the screen already; then nobody is listening.
            let _ = sender.send(plan);
        });
        receiver
    }

    #[cfg(test)]
    pub fn with_plan(plan: Plan) -> Self {
        Self {
            config: Config::default(),
            state: State::Ready(PlanView::new(plan)),
        }
    }

    #[cfg(test)]
    pub fn has_plan(&self) -> bool {
        matches!(self.state, State::Ready(_))
    }

    /// Moves background work along: starts the scan or the removal, and picks up their results.
    pub fn tick(&mut self) {
        let state = std::mem::replace(&mut self.state, State::NotStarted);
        self.state = match state {
            State::NotStarted => State::Scanning(Self::start(self.config.clone())),
            State::Scanning(receiver) => match receiver.try_recv() {
                Ok(Ok(plan)) => State::Ready(PlanView::new(plan)),
                Ok(Err(err)) => State::Failed(format!("Could not build the plan: {err}")),
                Err(TryRecvError::Empty) => State::Scanning(receiver),
                Err(TryRecvError::Disconnected) => {
                    State::Failed("the scan stopped unexpectedly".into())
                }
            },
            State::Removing(view) => match self.prepare() {
                Ok((rules, mounts, log)) => Self::start_removal(view.plan, rules, mounts, log),
                Err(err) => State::Failed(format!("Nothing was removed: {err}")),
            },
            State::RemovingHome {
                plan,
                receiver,
                stop,
                mut now,
            } => loop {
                match receiver.try_recv() {
                    Ok(Progress::Item(label)) => now = label,
                    Ok(Progress::Finished(mut outcomes, log)) => {
                        let root = plan.selected().filter(|item| item.path().is_none());
                        if stop.load(Ordering::Relaxed) {
                            outcomes.extend(root.map(|item| {
                                remove::not_attempted(item, "stopped before removing it")
                            }));
                            break State::Done {
                                outcomes,
                                scroll: 0,
                            };
                        }
                        if root.count() == 0 {
                            break State::Done {
                                outcomes,
                                scroll: 0,
                            };
                        }
                        break State::RootPending {
                            plan,
                            outcomes,
                            log,
                        };
                    }
                    Err(TryRecvError::Empty) => {
                        break State::RemovingHome {
                            plan,
                            receiver,
                            stop,
                            now,
                        };
                    }
                    Err(TryRecvError::Disconnected) => {
                        break State::Failed(
                            "The removal stopped unexpectedly; the operation log has what was removed."
                                .into(),
                        );
                    }
                }
            },
            state => state,
        };
    }

    /// Removes the home Items on a worker thread, so the screen keeps drawing.
    fn start_removal(plan: Plan, rules: Rules, mounts: Mounts, mut log: Log) -> State {
        let (sender, receiver) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let worker_plan = plan.clone();
        let worker_stop = Arc::clone(&stop);
        thread::spawn(move || {
            let mut progress = |done: usize, total: usize, label: &str| {
                let _ = sender.send(Progress::Item(format!("{label} ({} of {total})", done + 1)));
            };
            let outcomes = remove::remove_home(
                &worker_plan,
                &rules,
                &mounts,
                &mut log,
                &mut progress,
                &worker_stop,
            );
            let _ = sender.send(Progress::Finished(outcomes, log));
        });
        State::RemovingHome {
            plan,
            receiver,
            stop,
            now: String::new(),
        }
    }

    /// Returns whether the key was used.
    pub fn on_key(&mut self, key: KeyEvent) -> bool {
        let back = matches!(
            key.code,
            KeyCode::Char('n' | 'q' | 'h') | KeyCode::Esc | KeyCode::Left
        );
        if let State::RemovingHome { stop, .. } = &self.state
            && key.code == KeyCode::Esc
        {
            stop.store(true, Ordering::Relaxed);
            return true;
        }
        let state = std::mem::replace(&mut self.state, State::NotStarted);
        let (state, used) = match state {
            State::Ready(mut view) if key.code == KeyCode::Enter => {
                if view.plan.selected().next().is_some() {
                    refresh_in_use(&mut view.plan, &self.config);
                    (
                        State::Confirm {
                            view,
                            in_use: false,
                            scroll: 0,
                        },
                        true,
                    )
                } else {
                    (State::Ready(view), true)
                }
            }
            State::Ready(mut view) => {
                let used = view.on_key(key);
                (State::Ready(view), used)
            }
            // The In Use step takes another key (Y), so a double-tap or auto-repeat of y can't
            // pass it before it is even drawn.
            State::Confirm {
                mut view, in_use, ..
            } if key.code == KeyCode::Char(if in_use { 'Y' } else { 'y' }) => {
                refresh_in_use(&mut view.plan, &self.config);
                let any_in_use = view.plan.selected().any(|item| item.in_use);
                if any_in_use && !in_use {
                    (
                        State::Confirm {
                            view,
                            in_use: true,
                            scroll: 0,
                        },
                        true,
                    )
                } else {
                    (State::Removing(view), true)
                }
            }
            State::Confirm { view, .. } if back => (State::Ready(view), true),
            State::Confirm {
                view,
                in_use,
                scroll,
            } => (
                State::Confirm {
                    view,
                    in_use,
                    scroll: ui::scrolled(scroll, key),
                },
                true,
            ),
            state @ (State::Removing(_)
            | State::RemovingHome { .. }
            | State::RootPending { .. }) => (state, true),
            State::Done { .. } | State::Failed(_) if matches!(key.code, KeyCode::Enter) => {
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

    pub fn hint(&self) -> &'static str {
        match &self.state {
            State::Ready(_) => "↑↓/jk move · space select · a select category · enter remove…",
            State::Confirm { in_use: false, .. } => "↑↓ scroll · y remove · n back",
            State::Confirm { in_use: true, .. } => "↑↓ scroll · Y remove anyway · n back",
            State::RemovingHome { .. } => "esc stop after this Item",
            State::Done { .. } => "↑↓ scroll · enter scan again",
            State::Failed(_) => "enter scan again",
            State::NotStarted
            | State::Scanning(_)
            | State::Removing(_)
            | State::RootPending { .. } => "",
        }
    }

    /// Whether q leaves `clean` here; while confirming or removing, it doesn't.
    pub fn can_leave(&self) -> bool {
        match self.state {
            State::NotStarted
            | State::Scanning(_)
            | State::Ready(_)
            | State::Done { .. }
            | State::Failed(_) => true,
            State::Confirm { .. }
            | State::Removing(_)
            | State::RemovingHome { .. }
            | State::RootPending { .. } => false,
        }
    }

    #[cfg(test)]
    pub fn is_removing(&self) -> bool {
        matches!(self.state, State::Removing(_))
    }

    /// Whether the root commands are waiting for [`Screen::run_root`].
    pub fn needs_terminal(&self) -> bool {
        matches!(self.state, State::RootPending { .. })
    }

    /// Runs the confirmed root commands; `run_root` runs them with the TUI paused. Fails when
    /// the TUI couldn't be brought back afterwards; the result is logged either way.
    pub fn run_root(&mut self, run_root: &mut RunRoot) -> io::Result<()> {
        let state = std::mem::replace(&mut self.state, State::NotStarted);
        let State::RootPending {
            plan,
            mut outcomes,
            mut log,
        } = state
        else {
            self.state = state;
            return Ok(());
        };
        let (root, terminal) = remove::remove_root(&plan, &mut log, run_root);
        outcomes.extend(root);
        self.state = State::Done {
            outcomes,
            scroll: 0,
        };
        terminal
    }

    /// Fresh path rules and mount points, and the Operation Log. Nothing is removed without all
    /// three: bind mounts can only be told apart through the mount points.
    fn prepare(&self) -> Result<(Rules, Mounts, Log), String> {
        let home = dirs::home_dir().ok_or("the home directory could not be found")?;
        let rules = Rules::new(&home, ALLOWED_ROOTS, &self.config.protected_paths(&home))
            .map_err(|err| format!("resolving {}: {err}", home.display()))?;
        let mounts = Mounts::load()
            .map_err(|err| format!("reading the mount points from /proc/self/mountinfo: {err}"))?;
        let log = Log::open_default("clean")?;
        Ok((rules, mounts, log))
    }

    pub fn draw(&mut self, frame: &mut Frame, area: Rect) {
        let block = Block::bordered().title(" clean ");
        match &mut self.state {
            State::NotStarted | State::Scanning(_) => frame.render_widget(
                Paragraph::new("Measuring caches, the pacman cache and the journal…").block(block),
                area,
            ),
            State::Failed(err) => {
                frame.render_widget(Paragraph::new(err.as_str()).block(block), area)
            }
            State::Ready(view) => view.draw(frame, area, block),
            State::Confirm {
                view,
                in_use,
                scroll,
            } => draw_confirm(frame, area, block, &view.plan, *in_use, scroll),
            State::Removing(_) | State::RootPending { .. } => {
                frame.render_widget(Paragraph::new("Removing…").block(block), area)
            }
            State::RemovingHome { now, stop, .. } => {
                let text = if stop.load(Ordering::Relaxed) {
                    format!("Stopping after {now}…")
                } else {
                    format!("Removing {now}…")
                };
                frame.render_widget(Paragraph::new(text).block(block), area)
            }
            State::Done { outcomes, scroll } => {
                ui::draw_outcomes(frame, area, block, outcomes, scroll)
            }
        }
    }
}

fn draw_confirm(
    frame: &mut Frame,
    area: Rect,
    block: Block,
    plan: &Plan,
    in_use: bool,
    scroll: &mut u16,
) {
    let bold = Style::new().add_modifier(Modifier::BOLD);
    let selected: Vec<&Item> = plan.selected().collect();
    let width = usize::from(block.inner(area).width);
    let mut lines = Vec::new();
    if in_use {
        let count = selected.iter().filter(|item| item.in_use).count();
        lines.push(Line::styled(
            format!("{count} of these Items are in use by running programs."),
            bold,
        ));
        lines.push(Line::from(
            "Removing them can make those programs misbehave. Press Y (shift+y) to remove them anyway.",
        ));
    } else {
        lines.push(Line::styled(
            format!(
                "Remove these {} Items, freeing about {}?",
                selected.len(),
                size::format(plan.selected_size())
            ),
            bold,
        ));
        lines.push(Line::from("Caches and the trash are deleted permanently."));
        if selected.iter().any(|item| item.path().is_none()) {
            lines.push(Line::from(
                "For the pacman cache and the journal, vole pauses and sudo asks for your password.",
            ));
        }
    }
    lines.push(Line::default());
    for item in &selected {
        let mut spans = vec![Span::raw(format!("  {}", item.label))];
        if item.in_use {
            spans.push(Span::styled("  in use", bold));
        }
        lines.push(ui::right_aligned(spans, size::format(item.size), width));
    }
    ui::draw_scrolled(frame, area, block, lines, scroll);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Row {
    Category(usize),
    Item(usize, usize),
}

impl Row {
    fn category(self) -> usize {
        match self {
            Row::Category(c) | Row::Item(c, _) => c,
        }
    }
}

struct PlanView {
    plan: Plan,
    rows: Vec<Row>,
    list: ListState,
}

impl PlanView {
    fn new(plan: Plan) -> Self {
        let mut rows = Vec::new();
        for (c, category) in plan.categories.iter().enumerate() {
            rows.push(Row::Category(c));
            rows.extend((0..category.items.len()).map(|i| Row::Item(c, i)));
        }
        Self {
            plan,
            rows,
            list: ListState::default().with_selected(Some(0)),
        }
    }

    fn current(&self) -> Option<Row> {
        self.list.selected().and_then(|i| self.rows.get(i)).copied()
    }

    fn on_key(&mut self, key: KeyEvent) -> bool {
        let last = self.rows.len().saturating_sub(1);
        let index = self.list.selected().unwrap_or(0).min(last);
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => self.list.select(Some((index + 1).min(last))),
            KeyCode::Up | KeyCode::Char('k') => self.list.select(Some(index.saturating_sub(1))),
            KeyCode::Char(' ') => match self.current() {
                Some(Row::Item(c, i)) => self.plan.categories[c].toggle(i),
                Some(Row::Category(c)) => self.plan.categories[c].toggle_all(),
                None => {}
            },
            KeyCode::Char('a') => {
                if let Some(row) = self.current() {
                    self.plan.categories[row.category()].toggle_all();
                }
            }
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
        let lines: Vec<Line> = self.rows.iter().map(|&row| self.line(row, width)).collect();
        let list = List::new(lines)
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

    fn line(&self, row: Row, width: usize) -> Line<'static> {
        let dim = Style::new().add_modifier(Modifier::DIM);
        match row {
            Row::Category(c) => {
                let category = &self.plan.categories[c];
                let mut spans = vec![Span::styled(
                    category.name.to_string(),
                    Style::new().add_modifier(Modifier::BOLD),
                )];
                if let Some(note) = &category.note {
                    spans.push(Span::styled(format!("  {note}"), dim));
                } else if category.items.is_empty() {
                    spans.push(Span::styled("  nothing to clean", dim));
                }
                let size = if category.items.is_empty() {
                    String::new()
                } else {
                    size::format(category.size())
                };
                ui::right_aligned(spans, size, width)
            }
            Row::Item(c, i) => {
                let item = &self.plan.categories[c].items[i];
                let check = if item.selected { "[x]" } else { "[ ]" };
                let mut spans = vec![Span::raw(format!("  {check} {}", item.label))];
                if item.is_regenerable() {
                    spans.push(Span::styled("  regenerable", dim));
                }
                if item.in_use {
                    spans.push(Span::styled(
                        "  in use",
                        Style::new().add_modifier(Modifier::BOLD),
                    ));
                }
                ui::right_aligned(spans, size::format(item.size), width)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clean::plan::{Category, Item, Kind};

    fn screen() -> Screen {
        let mut first = Category::new("First");
        first.items = vec![
            Item::new("a", Kind::Regenerable("/a".into()), 10),
            Item::new("b", Kind::Regenerable("/b".into()), 20),
        ];
        let mut second = Category::new("Second");
        second.items = vec![Item::new("c", Kind::Trash("/c".into()), 40)];
        Screen::with_plan(Plan {
            categories: vec![first, second],
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
    fn space_selects_and_deselects_an_item() {
        let mut screen = screen();
        press(&mut screen, KeyCode::Down);
        press(&mut screen, KeyCode::Down);
        press(&mut screen, KeyCode::Char(' '));
        assert_eq!(selected_size(&screen), 20);
        press(&mut screen, KeyCode::Char(' '));
        assert_eq!(selected_size(&screen), 0);
    }

    #[test]
    fn selects_a_whole_category() {
        let mut screen = screen();
        press(&mut screen, KeyCode::Char('j'));
        press(&mut screen, KeyCode::Char('a'));
        assert_eq!(selected_size(&screen), 30);
        // Space on a Category header toggles the whole Category too.
        for _ in 0..2 {
            press(&mut screen, KeyCode::Char('j'));
        }
        press(&mut screen, KeyCode::Char(' '));
        assert_eq!(selected_size(&screen), 70);
    }

    #[test]
    fn movement_stays_in_bounds() {
        let mut screen = screen();
        press(&mut screen, KeyCode::Up);
        for _ in 0..10 {
            press(&mut screen, KeyCode::Down);
        }
        press(&mut screen, KeyCode::Char(' '));
        assert_eq!(selected_size(&screen), 40);
    }

    fn is_confirming(screen: &Screen, step: bool) -> bool {
        matches!(screen.state, State::Confirm { in_use, .. } if in_use == step)
    }

    #[test]
    fn enter_asks_for_confirmation_only_with_a_selection() {
        let mut screen = screen();
        assert!(press(&mut screen, KeyCode::Enter));
        assert!(screen.has_plan());
        press(&mut screen, KeyCode::Char('a'));
        press(&mut screen, KeyCode::Enter);
        assert!(is_confirming(&screen, false));
        // Backing out keeps the selection, and q goes back instead of leaving.
        assert!(!screen.can_leave());
        assert!(press(&mut screen, KeyCode::Char('q')));
        assert!(screen.can_leave());
        assert_eq!(selected_size(&screen), 30);
        press(&mut screen, KeyCode::Enter);
        press(&mut screen, KeyCode::Char('y'));
        assert!(screen.is_removing());
    }

    #[test]
    fn in_use_items_need_a_second_confirmation() {
        let mut screen = screen();
        if let State::Ready(view) = &mut screen.state {
            view.plan.categories[1].items[0].in_use = true;
        }
        for _ in 0..4 {
            press(&mut screen, KeyCode::Char('j'));
        }
        press(&mut screen, KeyCode::Char(' '));
        press(&mut screen, KeyCode::Enter);
        press(&mut screen, KeyCode::Char('y'));
        assert!(is_confirming(&screen, true));
        press(&mut screen, KeyCode::Esc);
        assert!(screen.has_plan());
        press(&mut screen, KeyCode::Enter);
        press(&mut screen, KeyCode::Char('y'));
        // A second y (a double-tap, auto-repeat) doesn't pass the In Use step.
        press(&mut screen, KeyCode::Char('y'));
        assert!(is_confirming(&screen, true));
        press(&mut screen, KeyCode::Char('Y'));
        assert!(screen.is_removing());
    }

    #[test]
    fn long_lists_scroll_within_bounds() {
        let mut first = Category::new("Many");
        first.items = (0..40)
            .map(|i| {
                Item::new(
                    format!("item {i}"),
                    Kind::Regenerable(format!("/{i}").into()),
                    1,
                )
            })
            .collect();
        let mut screen = Screen::with_plan(Plan {
            categories: vec![first],
        });
        press(&mut screen, KeyCode::Char('a'));
        press(&mut screen, KeyCode::Enter);
        for _ in 0..100 {
            press(&mut screen, KeyCode::Char('j'));
        }
        let backend = ratatui::backend::TestBackend::new(40, 12);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| screen.draw(frame, frame.area()))
            .unwrap();
        let State::Confirm { scroll, .. } = screen.state else {
            panic!("not confirming");
        };
        // 2 heading lines, a blank one and 40 Items, in 10 rows.
        assert_eq!(scroll, 43 - 10);
        let last_row: String = (0..40)
            .map(|x| terminal.backend().buffer()[(x, 10)].symbol().to_string())
            .collect();
        assert!(last_row.contains("item 39"), "{last_row}");
    }

    #[test]
    fn keys_do_nothing_while_removing() {
        let mut screen = screen();
        press(&mut screen, KeyCode::Char('a'));
        press(&mut screen, KeyCode::Enter);
        press(&mut screen, KeyCode::Char('y'));
        assert!(press(&mut screen, KeyCode::Char('q')));
        assert!(screen.is_removing());
    }

    #[test]
    fn leaving_keys_are_not_used() {
        let mut screen = screen();
        assert!(!press(&mut screen, KeyCode::Char('q')));
        assert!(!press(&mut screen, KeyCode::Esc));
    }
}
