//! The `clean` screens: scanning, then the Plan with selection.

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
use crate::config::Config;
use crate::size;

pub struct Screen {
    state: State,
}

enum State {
    Scanning(Receiver<Result<Plan, String>>),
    Ready(PlanView),
    Failed(String),
}

impl Screen {
    /// Starts building the Plan in the background.
    pub fn start(config: &Config) -> Self {
        let (sender, receiver) = mpsc::channel();
        let config = config.clone();
        thread::spawn(move || {
            let plan = match dirs::home_dir() {
                Some(home) => build_plan(&home, &config).map_err(|err| format!("{err:#}")),
                None => Err("the home directory could not be found".into()),
            };
            // The user may have left the screen already; then nobody is listening.
            let _ = sender.send(plan);
        });
        Self {
            state: State::Scanning(receiver),
        }
    }

    #[cfg(test)]
    pub fn with_plan(plan: Plan) -> Self {
        Self {
            state: State::Ready(PlanView::new(plan)),
        }
    }

    /// Picks up the Plan once the scan is done.
    pub fn tick(&mut self) {
        let State::Scanning(receiver) = &self.state else {
            return;
        };
        self.state = match receiver.try_recv() {
            Ok(Ok(plan)) => State::Ready(PlanView::new(plan)),
            Ok(Err(err)) => State::Failed(err),
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => {
                State::Failed("the scan stopped unexpectedly".into())
            }
        };
    }

    /// Returns whether the key was used.
    pub fn on_key(&mut self, key: KeyEvent) -> bool {
        match &mut self.state {
            State::Ready(view) => view.on_key(key),
            State::Scanning(_) | State::Failed(_) => false,
        }
    }

    pub fn hint(&self) -> &'static str {
        match self.state {
            State::Ready(_) => "↑↓/jk move · space select · a select category",
            State::Scanning(_) | State::Failed(_) => "",
        }
    }

    pub fn draw(&mut self, frame: &mut Frame, area: Rect) {
        let block = Block::bordered().title(" clean ");
        match &mut self.state {
            State::Scanning(_) => frame.render_widget(
                Paragraph::new("Measuring caches, the pacman cache and the journal…").block(block),
                area,
            ),
            State::Failed(err) => frame.render_widget(
                Paragraph::new(format!("Could not build the plan: {err}")).block(block),
                area,
            ),
            State::Ready(view) => view.draw(frame, area, block),
        }
    }
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
            " Selected {} of {} · nothing is removed yet",
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
    let used: usize = spans.iter().map(|span| span.content.chars().count()).sum();
    let gap = width.saturating_sub(used + right.chars().count()).max(1);
    spans.push(Span::raw(" ".repeat(gap)));
    spans.push(Span::raw(right));
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
    fn leaving_keys_are_not_used() {
        let mut screen = screen();
        assert!(!press(&mut screen, KeyCode::Char('q')));
        assert!(!press(&mut screen, KeyCode::Esc));
    }
}
