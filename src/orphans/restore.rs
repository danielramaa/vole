//! `restore`: putting configs from a Backup back where they were.

use std::io;

use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, List, ListItem, ListState, Paragraph};

use super::backup::{self, Backup, Entry};
use super::rules;
use crate::app::{Action, ModuleScreen, Tick};
use crate::config::Config;
use crate::log::{Log, Outcome};
use crate::term::RunRoot;
use crate::{date, size, ui};

pub struct Screen {
    config: Config,
    state: State,
}

enum State {
    /// The Backups are listed on the first tick, so creating the screen touches nothing.
    NotStarted,
    Ready(View),
    Confirm {
        view: View,
        scroll: u16,
    },
    Done {
        outcomes: Vec<Outcome>,
        /// What went wrong besides putting configs back, like a Backup's list not updated.
        notes: Vec<String>,
        scroll: u16,
    },
    Failed(String),
}

impl Screen {
    pub fn new(config: &Config) -> Self {
        Self {
            config: config.clone(),
            state: State::NotStarted,
        }
    }

    #[cfg(test)]
    pub fn has_list(&self) -> bool {
        matches!(self.state, State::Ready(_))
    }

    #[cfg(test)]
    pub fn with_backups(backups: Vec<Backup>) -> Self {
        Self {
            config: Config::default(),
            state: State::Ready(View::new(backups)),
        }
    }

    /// Puts the selected configs back, each Backup's list then updated. Nothing moves without the
    /// path rules and the Operation Log, where each is written.
    fn put_back(&self, view: &View) -> State {
        let prepared = (|| -> Result<_, String> {
            let home = dirs::home_dir().ok_or("the home directory could not be found")?;
            let rules = rules(&home, &self.config)
                .map_err(|err| format!("resolving {}: {err}", home.display()))?;
            Ok((home, rules, Log::open_default("restore")?))
        })();
        let (home, rules, mut log) = match prepared {
            Ok(prepared) => prepared,
            Err(err) => return State::Failed(format!("Nothing was put back: {err}")),
        };
        let mut outcomes = Vec::new();
        let mut notes = Vec::new();
        for listed in &view.backups {
            let mut restored: Vec<Entry> = Vec::new();
            for (entry, _) in listed.entries().filter(|(_, selected)| *selected) {
                let label = format!("~/{}", entry.path.display());
                let from = listed.backup.dir.join(&entry.path).display().to_string();
                let result = backup::put_back(&home, &rules, &listed.backup.dir, entry);
                let to = match &result {
                    Ok(to) => to.display().to_string(),
                    Err(_) => home.join(&entry.path).display().to_string(),
                };
                if result.is_ok() {
                    restored.push(entry.clone());
                }
                let result = result.map(|_| ());
                let outcome =
                    log.record_restored(&label, "orphaned config", entry.size, &from, &to, result);
                outcomes.push(outcome);
            }
            if !restored.is_empty()
                && let Err(err) = backup::forget(&listed.backup.dir, &restored)
            {
                // Put back, but still listed: the list says more than the Backup holds.
                notes.push(format!(
                    "the list of {} could not be updated, so it still names what was put back: {err}",
                    listed.backup.dir.display()
                ));
            }
        }
        State::Done {
            outcomes,
            notes,
            scroll: 0,
        }
    }
}

impl ModuleScreen for Screen {
    /// Lists the Backups the first time.
    fn tick(&mut self) -> Tick {
        if let State::NotStarted = self.state {
            self.state = match dirs::home_dir() {
                Some(home) => State::Ready(View::new(backup::list(&home))),
                None => State::Failed("the home directory could not be found".into()),
            };
        }
        Tick::Nothing
    }

    fn on_key(&mut self, key: KeyEvent) -> Action {
        let back = matches!(
            key.code,
            KeyCode::Char('n' | 'q' | 'h') | KeyCode::Esc | KeyCode::Left
        );
        let state = std::mem::replace(&mut self.state, State::NotStarted);
        let (state, used) = match state {
            State::Ready(view) if key.code == KeyCode::Enter => {
                if view.selected().next().is_some() {
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
                (self.put_back(&view), true)
            }
            State::Confirm { view, .. } if back => (State::Ready(view), true),
            State::Confirm { view, scroll } => {
                let scroll = ui::scrolled(scroll, key);
                (State::Confirm { view, scroll }, true)
            }
            State::Done { .. } | State::Failed(_) if key.code == KeyCode::Enter => {
                (State::NotStarted, true)
            }
            State::Done {
                outcomes,
                notes,
                scroll,
            } => {
                let next = ui::scrolled(scroll, key);
                let done = State::Done {
                    outcomes,
                    notes,
                    scroll: next,
                };
                (done, next != scroll)
            }
            state => (state, false),
        };
        self.state = state;
        if used { Action::Used } else { Action::Unused }
    }

    fn hint(&self) -> &'static str {
        match self.state {
            State::Ready(_) => "↑↓/jk move · space select · a select Backup · enter put back…",
            State::Confirm { .. } => "↑↓ scroll · y put back · n back",
            State::Done { .. } => "↑↓ scroll · enter list again",
            State::Failed(_) => "enter list again",
            State::NotStarted => "",
        }
    }

    /// Whether q leaves `restore` here; while confirming, it goes back instead.
    fn can_leave(&self) -> bool {
        match self.state {
            State::NotStarted | State::Ready(_) | State::Done { .. } | State::Failed(_) => true,
            State::Confirm { .. } => false,
        }
    }

    fn run_root(&mut self, _: &mut RunRoot) -> io::Result<()> {
        Ok(())
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) {
        let block = Block::bordered().title(" restore ");
        match &mut self.state {
            State::NotStarted => {
                frame.render_widget(Paragraph::new("Listing the Backups…").block(block), area)
            }
            State::Failed(err) => {
                frame.render_widget(Paragraph::new(err.as_str()).block(block), area)
            }
            State::Ready(view) => view.draw(frame, area, block),
            State::Confirm { view, scroll } => draw_confirm(frame, area, block, view, scroll),
            State::Done {
                outcomes,
                notes,
                scroll,
            } => draw_done(frame, area, block, outcomes, notes, scroll),
        }
    }
}

fn draw_confirm(frame: &mut Frame, area: Rect, block: Block, view: &View, scroll: &mut u16) {
    let width = usize::from(block.inner(area).width);
    let selected: Vec<&Entry> = view.selected().collect();
    let question = match selected.len() {
        1 => "Put this config back where it was?".to_string(),
        count => format!("Put these {count} configs back where they were?"),
    };
    let mut lines = vec![
        Line::styled(question, Style::new().add_modifier(Modifier::BOLD)),
        Line::from("Anything there now stays: a config whose place is taken stays in its Backup."),
        Line::default(),
    ];
    for entry in selected {
        let label = vec![Span::raw(format!("  ~/{}", entry.path.display()))];
        lines.push(ui::right_aligned(label, size::format(entry.size), width));
    }
    ui::draw_scrolled(frame, area, block, lines, scroll);
}

fn draw_done(
    frame: &mut Frame,
    area: Rect,
    block: Block,
    outcomes: &[Outcome],
    notes: &[String],
    scroll: &mut u16,
) {
    let back = outcomes.iter().filter(|o| o.error.is_none()).count();
    let failed = outcomes.len() - back;
    let mut summary = match back {
        1 => "Put back 1 config".to_string(),
        count => format!("Put back {count} configs"),
    };
    if failed > 0 {
        summary.push_str(&format!(" · {failed} failed"));
    }
    let dim = Style::new().add_modifier(Modifier::DIM);
    let mut lines = vec![
        Line::styled(summary, Style::new().add_modifier(Modifier::BOLD)),
        Line::default(),
    ];
    for outcome in outcomes {
        match &outcome.error {
            None => lines.push(Line::from(format!("  ✓ {}", outcome.label))),
            Some(err) => {
                lines.push(Line::from(format!("  ✗ {}", outcome.label)));
                lines.push(Line::styled(format!("      {err}"), dim));
            }
        }
        if let Some(err) = &outcome.log_error {
            lines.push(Line::styled(format!("      ! {err}"), dim));
        }
    }
    for note in notes {
        lines.push(Line::styled(format!("  ! {note}"), dim));
    }
    ui::draw_scrolled(frame, area, block, lines, scroll);
}

/// A Backup with which of its configs are selected.
struct Listed {
    backup: Backup,
    selected: Vec<bool>,
}

impl Listed {
    fn entries(&self) -> impl Iterator<Item = (&Entry, bool)> {
        self.backup
            .entries
            .iter()
            .zip(self.selected.iter().copied())
    }

    /// Selects every config, or deselects them all when they already are.
    fn toggle_all(&mut self) {
        let select = !self.selected.iter().all(|&selected| selected);
        self.selected.fill(select);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Row {
    Backup(usize),
    Entry(usize, usize),
}

impl Row {
    fn backup(self) -> usize {
        match self {
            Row::Backup(b) | Row::Entry(b, _) => b,
        }
    }
}

/// The Backups, newest first, each with its configs under it.
struct View {
    backups: Vec<Listed>,
    rows: Vec<Row>,
    list: ListState,
}

impl View {
    fn new(backups: Vec<Backup>) -> Self {
        let mut backups: Vec<Listed> = backups
            .into_iter()
            .map(|backup| Listed {
                selected: vec![false; backup.entries.len()],
                backup,
            })
            .collect();
        backups.reverse();
        let mut rows = Vec::new();
        for (b, listed) in backups.iter().enumerate() {
            rows.push(Row::Backup(b));
            rows.extend((0..listed.backup.entries.len()).map(|e| Row::Entry(b, e)));
        }
        Self {
            backups,
            rows,
            list: ListState::default().with_selected(Some(0)),
        }
    }

    fn selected(&self) -> impl Iterator<Item = &Entry> {
        self.backups.iter().flat_map(|listed| {
            listed
                .entries()
                .filter(|(_, selected)| *selected)
                .map(|(entry, _)| entry)
        })
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
                Some(Row::Entry(b, e)) => {
                    let selected = &mut self.backups[b].selected[e];
                    *selected = !*selected;
                }
                Some(Row::Backup(b)) => self.backups[b].toggle_all(),
                None => {}
            },
            KeyCode::Char('a') => {
                if let Some(row) = self.current() {
                    self.backups[row.backup()].toggle_all();
                }
            }
            _ => return false,
        }
        true
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect, block: Block) {
        if self.backups.is_empty() {
            let text = "No Backups. vole orphans moves the configs you remove into one.";
            frame.render_widget(Paragraph::new(text).block(block), area);
            return;
        }
        let inner = block.inner(area);
        frame.render_widget(block, area);
        let [list_area, total_area] =
            Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(inner);
        // Room for the highlight symbol.
        let width = usize::from(list_area.width).saturating_sub(2);
        let lines: Vec<ListItem> = self
            .rows
            .iter()
            .map(|&row| ListItem::new(self.line(row, width)))
            .collect();
        let list = List::new(lines)
            .highlight_style(Style::new().add_modifier(Modifier::REVERSED))
            .highlight_symbol("> ");
        frame.render_stateful_widget(list, list_area, &mut self.list);
        let count = self.selected().count();
        let total = format!(
            " {count} selected, {}",
            size::format(self.selected().map(|entry| entry.size).sum())
        );
        frame.render_widget(
            Line::styled(total, Style::new().add_modifier(Modifier::BOLD)),
            total_area,
        );
    }

    fn line(&self, row: Row, width: usize) -> Line<'static> {
        let dim = Style::new().add_modifier(Modifier::DIM);
        match row {
            Row::Backup(b) => {
                let backup = &self.backups[b].backup;
                let configs = match backup.entries.len() {
                    1 => "1 config".to_string(),
                    count => format!("{count} configs"),
                };
                let spans = vec![
                    Span::styled(
                        format!("Backup from {}", date::day_and_time(backup.created)),
                        Style::new().add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(format!("  {configs}"), dim),
                ];
                let total = backup.entries.iter().map(|entry| entry.size).sum();
                ui::right_aligned(spans, size::format(total), width)
            }
            Row::Entry(b, e) => {
                let listed = &self.backups[b];
                let entry = &listed.backup.entries[e];
                let check = if listed.selected[e] { "[x]" } else { "[ ]" };
                let spans = vec![Span::raw(format!("  {check} ~/{}", entry.path.display()))];
                ui::right_aligned(spans, size::format(entry.size), width)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, UNIX_EPOCH};

    fn backups() -> Vec<Backup> {
        let at = |secs| UNIX_EPOCH + Duration::from_secs(secs);
        let entry = |path: &str, size| Entry {
            path: path.into(),
            size,
        };
        vec![
            Backup {
                dir: "/home/me/.local/share/vole/backups/2026-08-01T00-00-00Z".into(),
                created: at(1_785_542_400),
                entries: vec![entry(".config/old", 1024)],
            },
            Backup {
                dir: "/home/me/.local/share/vole/backups/2026-10-05T13-30-00Z".into(),
                created: at(1_791_207_000),
                entries: vec![entry(".config/gone", 2048), entry(".gonerc", 10)],
            },
        ]
    }

    fn press(screen: &mut Screen, code: KeyCode) -> bool {
        screen.on_key(KeyEvent::from(code)) == Action::Used
    }

    fn render(screen: &mut Screen) -> String {
        let backend = ratatui::backend::TestBackend::new(80, 10);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| screen.draw(frame, frame.area()))
            .unwrap();
        let buffer = terminal.backend().buffer();
        (0..10)
            .map(|y| (0..80).map(|x| buffer[(x, y)].symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn lists_backups_newest_first_with_their_configs() {
        let mut screen = Screen::with_backups(backups());
        let text = render(&mut screen);
        let newest = text
            .find("Backup from 5 Oct 2026, 13:30 UTC  2 configs")
            .unwrap();
        let oldest = text
            .find("Backup from 1 Aug 2026, 00:00 UTC  1 config")
            .unwrap();
        assert!(newest < oldest, "{text}");
        assert!(text.contains("[ ] ~/.config/gone"));
    }

    #[test]
    fn selects_configs_or_whole_backups_and_asks() {
        let mut screen = Screen::with_backups(backups());
        assert!(press(&mut screen, KeyCode::Enter));
        assert!(matches!(screen.state, State::Ready(_)));
        // Space on a Backup selects all its configs.
        press(&mut screen, KeyCode::Char(' '));
        press(&mut screen, KeyCode::Char('j'));
        press(&mut screen, KeyCode::Char(' '));
        press(&mut screen, KeyCode::Enter);
        assert!(!screen.can_leave());
        let text = render(&mut screen);
        assert!(
            text.contains("Put this config back where it was?"),
            "{text}"
        );
        assert!(text.contains("~/.gonerc"));
        assert!(!text.contains("~/.config/gone "));
        // Back keeps the selection.
        press(&mut screen, KeyCode::Char('n'));
        assert!(screen.can_leave());
        let State::Ready(view) = &screen.state else {
            panic!("not listing");
        };
        assert_eq!(view.selected().count(), 1);
    }

    #[test]
    fn notes_are_not_counted_as_failures() {
        let mut screen = Screen::with_backups(Vec::new());
        screen.state = State::Done {
            outcomes: vec![Outcome {
                label: "~/.config/gone".into(),
                size: 1,
                error: None,
                log_error: None,
                moved: None,
            }],
            notes: vec!["the list of /b could not be updated".into()],
            scroll: 0,
        };
        let text = render(&mut screen);
        assert!(text.contains("Put back 1 config"), "{text}");
        assert!(!text.contains("failed"));
        assert!(text.contains("! the list of /b could not be updated"));
    }

    #[test]
    fn says_when_there_is_nothing_to_restore() {
        let mut screen = Screen::with_backups(Vec::new());
        assert!(render(&mut screen).contains("No Backups."));
        assert!(!press(&mut screen, KeyCode::Char('q')));
    }
}
