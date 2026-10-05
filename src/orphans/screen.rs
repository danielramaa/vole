//! The `orphans` screens: looking for Orphaned Packages, and the Plan with selection.

use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;

use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, List, ListState, Paragraph};

use super::build_plan;
use super::plan::Plan;
use crate::{size, ui};

pub struct Screen {
    state: State,
}

enum State {
    /// The scan starts on the first tick, so creating the screen touches nothing.
    NotStarted,
    Scanning(Receiver<Result<Plan, String>>),
    Ready(PlanView),
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

    /// Moves background work along: starts the scan and picks up its result.
    pub fn tick(&mut self) {
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
    pub fn on_key(&mut self, key: KeyEvent) -> bool {
        match &mut self.state {
            State::Ready(view) => view.on_key(key),
            State::Failed(_) if key.code == KeyCode::Enter => {
                self.state = State::NotStarted;
                true
            }
            State::NotStarted | State::Scanning(_) | State::Failed(_) => false,
        }
    }

    pub fn hint(&self) -> &'static str {
        match self.state {
            State::Ready(_) => "↑↓/jk move · space select · a select all",
            State::Failed(_) => "enter scan again",
            State::NotStarted | State::Scanning(_) => "",
        }
    }

    /// Whether q leaves `orphans` here.
    pub fn can_leave(&self) -> bool {
        true
    }

    pub fn draw(&mut self, frame: &mut Frame, area: Rect) {
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
        }
    }
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
        let mut lines = vec![self.heading(width)];
        lines.extend((0..self.plan.packages.len()).map(|i| self.line(i, width)));
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

    fn line(&self, index: usize, width: usize) -> Line<'static> {
        let package = &self.plan.packages[index];
        let check = if package.selected { "[x]" } else { "[ ]" };
        let spans = vec![Span::raw(format!("  {check} {}", package.name))];
        ui::right_aligned(spans, size::format(package.size), width)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orphans::plan::Package;

    fn screen() -> Screen {
        let package = |name: &str, size| Package {
            name: name.into(),
            size,
            selected: false,
        };
        Screen::with_plan(Plan {
            packages: vec![package("a", 10), package("b", 20)],
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

    #[test]
    fn leaving_keys_are_not_used() {
        let mut screen = screen();
        assert!(!press(&mut screen, KeyCode::Char('q')));
        assert!(!press(&mut screen, KeyCode::Esc));
        assert!(screen.can_leave());
    }
}
