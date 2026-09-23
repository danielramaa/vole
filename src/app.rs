use anyhow::Result;
use ratatui::DefaultTerminal;
use ratatui::Frame;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, List, ListState, Paragraph};

use crate::config::Config;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Screen {
    Menu,
    Clean,
    Orphans,
    Analyze,
    Status,
}

impl Screen {
    const MODULES: [Screen; 4] = [
        Screen::Clean,
        Screen::Orphans,
        Screen::Analyze,
        Screen::Status,
    ];

    fn title(self) -> &'static str {
        match self {
            Screen::Menu => "vole",
            Screen::Clean => "clean",
            Screen::Orphans => "orphans",
            Screen::Analyze => "analyze",
            Screen::Status => "status",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Screen::Menu => "",
            Screen::Clean => "Caches, pacman cache, journal and trash",
            Screen::Orphans => "Orphaned packages and configs",
            Screen::Analyze => "Interactive disk explorer",
            Screen::Status => "Real-time system dashboard",
        }
    }
}

pub struct App {
    screen: Screen,
    /// Whether vole was opened on the menu; `q` on a module then goes back instead of quitting.
    started_on_menu: bool,
    menu: ListState,
    #[allow(dead_code)] // read by the modules from M1 on
    config: Config,
    quit: bool,
}

impl App {
    pub fn new(screen: Screen, config: Config) -> Self {
        Self {
            screen,
            started_on_menu: screen == Screen::Menu,
            menu: ListState::default().with_selected(Some(0)),
            config,
            quit: false,
        }
    }

    pub fn run(mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        while !self.quit {
            terminal.draw(|frame| self.draw(frame))?;
            if let Event::Key(key) = event::read()?
                && key.kind == KeyEventKind::Press
            {
                self.on_key(key);
            }
        }
        Ok(())
    }

    fn on_key(&mut self, key: KeyEvent) {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.quit = true;
            return;
        }
        match self.screen {
            Screen::Menu => match key.code {
                KeyCode::Char('q') | KeyCode::Esc => self.quit = true,
                KeyCode::Down | KeyCode::Char('j') => self.menu.select_next(),
                KeyCode::Up | KeyCode::Char('k') => self.menu.select_previous(),
                KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => {
                    let index = self
                        .menu
                        .selected()
                        .unwrap_or(0)
                        .min(Screen::MODULES.len() - 1);
                    self.screen = Screen::MODULES[index];
                }
                _ => {}
            },
            _ => match key.code {
                KeyCode::Char('q') | KeyCode::Esc | KeyCode::Left | KeyCode::Char('h') => {
                    if self.started_on_menu {
                        self.screen = Screen::Menu;
                    } else {
                        self.quit = true;
                    }
                }
                _ => {}
            },
        }
    }

    fn draw(&mut self, frame: &mut Frame) {
        let [body, footer] =
            Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(frame.area());
        let block = Block::bordered().title(format!(" {} ", self.screen.title()));

        match self.screen {
            Screen::Menu => {
                let items = Screen::MODULES
                    .map(|m| Line::from(format!("{:<10}{}", m.title(), m.description())));
                let list = List::new(items)
                    .block(block)
                    .highlight_style(Style::new().add_modifier(Modifier::REVERSED))
                    .highlight_symbol("> ");
                frame.render_stateful_widget(list, body, &mut self.menu);
                frame.render_widget(Line::from(" ↑↓/jk move · enter/l open · q quit"), footer);
            }
            module => {
                frame.render_widget(
                    Paragraph::new(format!("{}\n\nComing soon.", module.description()))
                        .block(block),
                    body,
                );
                frame.render_widget(Line::from(" q/h back"), footer);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(app: &mut App, code: KeyCode) {
        app.on_key(KeyEvent::from(code));
    }

    #[test]
    fn menu_opens_module_and_goes_back() {
        let mut app = App::new(Screen::Menu, Config::default());
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.screen, Screen::Orphans);
        press(&mut app, KeyCode::Char('q'));
        assert_eq!(app.screen, Screen::Menu);
        assert!(!app.quit);
    }

    #[test]
    fn q_quits_when_opened_directly() {
        let mut app = App::new(Screen::Clean, Config::default());
        press(&mut app, KeyCode::Char('q'));
        assert!(app.quit);
    }
}
