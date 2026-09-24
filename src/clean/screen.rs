//! The `clean` screens: scanning, the Plan with selection, confirmation, and the result.

use std::io;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;

use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, List, ListState, Paragraph};

use super::build_plan;
use super::plan::{Item, Plan};
use super::remove::{self, Log, Outcome, RunRoot};
use super::scan::{ALLOWED_ROOTS, Mounts};
use crate::config::Config;
use crate::safety::Rules;
use crate::size;

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
    },
    /// Confirmed: the App runs the removal right after drawing this.
    Removing(PlanView),
    Done(Vec<Outcome>),
    Failed(String),
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

    /// Starts the scan, then picks up the Plan once it is done.
    pub fn tick(&mut self) {
        if let State::NotStarted = self.state {
            self.state = State::Scanning(Self::start(self.config.clone()));
        }
        let State::Scanning(receiver) = &self.state else {
            return;
        };
        self.state = match receiver.try_recv() {
            Ok(Ok(plan)) => State::Ready(PlanView::new(plan)),
            Ok(Err(err)) => State::Failed(format!("Could not build the plan: {err}")),
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => {
                State::Failed("the scan stopped unexpectedly".into())
            }
        };
    }

    /// Returns whether the key was used.
    pub fn on_key(&mut self, key: KeyEvent) -> bool {
        let back = matches!(
            key.code,
            KeyCode::Char('n' | 'q' | 'h') | KeyCode::Esc | KeyCode::Left
        );
        let yes = matches!(key.code, KeyCode::Char('y'));
        let state = std::mem::replace(&mut self.state, State::NotStarted);
        let (state, used) = match state {
            State::Ready(view) if key.code == KeyCode::Enter => {
                if view.plan.selected().next().is_some() {
                    (
                        State::Confirm {
                            view,
                            in_use: false,
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
            State::Confirm { view, in_use } if yes => {
                let any_in_use = view.plan.selected().any(|item| item.in_use);
                if any_in_use && !in_use {
                    (State::Confirm { view, in_use: true }, true)
                } else {
                    (State::Removing(view), true)
                }
            }
            State::Confirm { view, .. } if back => (State::Ready(view), true),
            state @ (State::Confirm { .. } | State::Removing(_)) => (state, true),
            State::Done(_) | State::Failed(_) if matches!(key.code, KeyCode::Enter) => {
                (State::NotStarted, true)
            }
            state => (state, false),
        };
        self.state = state;
        used
    }

    pub fn hint(&self) -> &'static str {
        match &self.state {
            State::Ready(_) => "↑↓/jk move · space select · a select category · enter remove…",
            State::Confirm { .. } => "y remove · n back",
            State::Done(_) | State::Failed(_) => "enter scan again",
            State::NotStarted | State::Scanning(_) | State::Removing(_) => "",
        }
    }

    pub fn is_removing(&self) -> bool {
        matches!(self.state, State::Removing(_))
    }

    /// Removes the confirmed Items. `run_root` runs the `sudo` commands with the TUI paused.
    /// Fails when the TUI couldn't be brought back afterwards; the result is logged either way.
    pub fn remove(&mut self, run_root: &mut RunRoot) -> io::Result<()> {
        let state = std::mem::replace(&mut self.state, State::NotStarted);
        let State::Removing(view) = state else {
            self.state = state;
            return Ok(());
        };
        let (state, terminal) = match self.prepare() {
            Ok((rules, mut log)) => {
                let (outcomes, terminal) =
                    remove::apply(&view.plan, &rules, &Mounts::load(), &mut log, run_root);
                (State::Done(outcomes), terminal)
            }
            Err(err) => (State::Failed(format!("Nothing was removed: {err}")), Ok(())),
        };
        self.state = state;
        terminal
    }

    /// Fresh path rules, and the Operation Log: nothing is removed if it can't be written.
    fn prepare(&self) -> Result<(Rules, Log), String> {
        let home = dirs::home_dir().ok_or("the home directory could not be found")?;
        let rules = Rules::new(&home, ALLOWED_ROOTS, &self.config.protected_paths(&home))
            .map_err(|err| format!("resolving {}: {err}", home.display()))?;
        let path = Log::path().ok_or("the operation log's folder could not be found")?;
        let log = Log::open(&path)
            .map_err(|err| format!("opening the operation log {}: {err}", path.display()))?;
        Ok((rules, log))
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
            State::Confirm { view, in_use } => {
                draw_confirm(frame, area, block, &view.plan, *in_use)
            }
            State::Removing(_) => {
                frame.render_widget(Paragraph::new("Removing…").block(block), area)
            }
            State::Done(outcomes) => draw_done(frame, area, block, outcomes),
        }
    }
}

fn draw_confirm(frame: &mut Frame, area: Rect, block: Block, plan: &Plan, in_use: bool) {
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
            "Removing them can make those programs misbehave. Remove them anyway?",
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
        lines.push(right_aligned(spans, size::format(item.size), width));
    }
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

fn draw_done(frame: &mut Frame, area: Rect, block: Block, outcomes: &[Outcome]) {
    let width = usize::from(block.inner(area).width);
    let freed: u64 = outcomes
        .iter()
        .filter(|o| o.error.is_none())
        .map(|o| o.size)
        .sum();
    let failed = outcomes.iter().filter(|o| o.error.is_some()).count();
    let mut summary = format!("Freed about {}", size::format(freed));
    if failed > 0 {
        summary.push_str(&format!(" · {failed} failed"));
    }
    let mut lines = vec![
        Line::styled(summary, Style::new().add_modifier(Modifier::BOLD)),
        Line::default(),
    ];
    for outcome in outcomes {
        match &outcome.error {
            None => lines.push(right_aligned(
                vec![Span::raw(format!("  ✓ {}", outcome.label))],
                size::format(outcome.size),
                width,
            )),
            Some(err) => {
                lines.push(Line::from(format!("  ✗ {}", outcome.label)));
                lines.push(Line::styled(
                    format!("      {err}"),
                    Style::new().add_modifier(Modifier::DIM),
                ));
            }
        }
    }
    frame.render_widget(Paragraph::new(lines).block(block), area);
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
                Some(Row::Item(c, i)) => {
                    let item = &mut self.plan.categories[c].items[i];
                    item.selected = !item.selected;
                }
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
                right_aligned(spans, size, width)
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
                right_aligned(spans, size::format(item.size), width)
            }
        }
    }
}

/// `spans` on the left, `right` at the right edge of `width`.
fn right_aligned(mut spans: Vec<Span<'static>>, right: String, width: usize) -> Line<'static> {
    // Terminal columns, not chars: wide characters take two.
    let right = Span::raw(right);
    let used: usize = spans.iter().map(Span::width).sum();
    let gap = width.saturating_sub(used + right.width()).max(1);
    spans.push(Span::raw(" ".repeat(gap)));
    spans.push(right);
    Line::from(spans)
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

    #[test]
    fn sizes_line_up_with_wide_characters() {
        let line = right_aligned(vec![Span::raw("~/.cache/日本")], "1 B".into(), 20);
        assert_eq!(line.width(), 20);
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
        assert!(press(&mut screen, KeyCode::Char('q')));
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
        press(&mut screen, KeyCode::Char('y'));
        assert!(screen.is_removing());
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
