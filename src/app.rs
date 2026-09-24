use std::time::Duration;

use anyhow::Result;
use clap::ValueEnum;
use ratatui::DefaultTerminal;
use ratatui::Frame;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, List, ListState, Paragraph};

use crate::clean;
use crate::config::Config;

/// The modules of vole. The doc comments are used both for `--help` and for the menu.
#[derive(ValueEnum, Clone, Copy, PartialEq, Eq, Debug)]
pub enum Module {
    /// Caches, pacman cache, journal and trash
    Clean,
    /// Orphaned packages and configs
    Orphans,
    /// Interactive disk explorer
    Analyze,
    /// Real-time system dashboard
    Status,
}

impl Module {
    fn all() -> &'static [Module] {
        Module::value_variants()
    }

    fn name(self) -> String {
        self.to_possible_value()
            .map(|v| v.get_name().to_owned())
            .unwrap_or_default()
    }

    fn description(self) -> String {
        self.to_possible_value()
            .and_then(|v| v.get_help().map(ToString::to_string))
            .unwrap_or_default()
    }
}

pub struct App {
    /// The open module, or `None` for the menu.
    module: Option<Module>,
    /// Whether vole was opened on the menu; leaving a module then goes back instead of quitting.
    started_on_menu: bool,
    menu: ListState,
    config: Config,
    /// The `clean` screen, while `clean` is open.
    clean: Option<clean::Screen>,
    quit: bool,
}

impl App {
    pub fn new(module: Option<Module>, config: Config) -> Self {
        let mut app = Self {
            module: None,
            started_on_menu: module.is_none(),
            menu: ListState::default().with_selected(Some(0)),
            config,
            clean: None,
            quit: false,
        };
        app.open(module);
        app
    }

    /// Opens `module`, or the menu for `None`.
    fn open(&mut self, module: Option<Module>) {
        self.module = module;
        self.clean = (module == Some(Module::Clean)).then(|| clean::Screen::start(&self.config));
    }

    pub fn run(mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        while !self.quit {
            if let Some(clean) = &mut self.clean {
                clean.tick();
            }
            terminal.draw(|frame| self.draw(frame))?;
            // Wake up now and then, so background work (the clean scan) shows up.
            if event::poll(Duration::from_millis(100))?
                && let Event::Key(key) = event::read()?
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
        match self.module {
            None => self.on_menu_key(key),
            Some(module) => {
                // The module gets the key first, so it can use h/l, Esc, etc. for its own
                // navigation; only unhandled keys leave the module.
                if !self.on_module_key(module, key) {
                    self.on_leave_key(key);
                }
            }
        }
    }

    fn on_menu_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => self.quit = true,
            KeyCode::Down | KeyCode::Char('j') => self.menu.select_next(),
            KeyCode::Up | KeyCode::Char('k') => self.menu.select_previous(),
            KeyCode::Enter | KeyCode::Right | KeyCode::Char('l') => {
                let modules = Module::all();
                let index = self.menu.selected().unwrap_or(0).min(modules.len() - 1);
                self.open(Some(modules[index]));
            }
            _ => {}
        }
    }

    /// Keys handled inside a module. Returns whether the key was used.
    fn on_module_key(&mut self, module: Module, key: KeyEvent) -> bool {
        match module {
            Module::Clean => self.clean.as_mut().is_some_and(|clean| clean.on_key(key)),
            Module::Orphans | Module::Analyze | Module::Status => false,
        }
    }

    fn on_leave_key(&mut self, key: KeyEvent) {
        if matches!(
            key.code,
            KeyCode::Char('q') | KeyCode::Esc | KeyCode::Left | KeyCode::Char('h')
        ) {
            if self.started_on_menu {
                self.open(None);
            } else {
                self.quit = true;
            }
        }
    }

    fn draw(&mut self, frame: &mut Frame) {
        let [body, footer] =
            Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(frame.area());

        match self.module {
            None => {
                let items = Module::all()
                    .iter()
                    .map(|m| Line::from(format!("{:<10}{}", m.name(), m.description())));
                let list = List::new(items)
                    .block(Block::bordered().title(" vole "))
                    .highlight_style(Style::new().add_modifier(Modifier::REVERSED))
                    .highlight_symbol("> ");
                frame.render_stateful_widget(list, body, &mut self.menu);
                frame.render_widget(Line::from(" ↑↓/jk move · enter/l open · q quit"), footer);
            }
            Some(module) => {
                let mut hint = String::from(" ");
                match (module, &mut self.clean) {
                    (Module::Clean, Some(clean)) => {
                        clean.draw(frame, body);
                        if !clean.hint().is_empty() {
                            hint.push_str(clean.hint());
                            hint.push_str(" · ");
                        }
                    }
                    _ => frame.render_widget(
                        Paragraph::new(format!("{}\n\nComing soon.", module.description()))
                            .block(Block::bordered().title(format!(" {} ", module.name()))),
                        body,
                    ),
                }
                hint.push_str(if self.started_on_menu {
                    "q/h back"
                } else {
                    "q quit"
                });
                frame.render_widget(Line::from(hint), footer);
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
        let mut app = App::new(None, Config::default());
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.module, Some(Module::Orphans));
        press(&mut app, KeyCode::Char('q'));
        assert_eq!(app.module, None);
        assert!(!app.quit);
    }

    #[test]
    fn q_quits_when_opened_directly() {
        let mut app = App::new(Some(Module::Status), Config::default());
        press(&mut app, KeyCode::Char('q'));
        assert!(app.quit);
    }

    #[test]
    fn every_module_has_a_name_and_description() {
        for module in Module::all() {
            assert!(!module.name().is_empty());
            assert!(!module.description().is_empty());
        }
    }
}
