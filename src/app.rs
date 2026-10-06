use std::io;
use std::time::Duration;

use anyhow::Result;
use clap::ValueEnum;
use ratatui::DefaultTerminal;
use ratatui::Frame;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, List, ListState, Paragraph};

use crate::config::Config;
use crate::term::{self, RunRoot};
use crate::{analyze, clean, orphans};

/// The modules of vole. The doc comments are used both for `--help` and for the menu.
#[derive(ValueEnum, Clone, Copy, PartialEq, Eq, Debug)]
pub enum Module {
    /// Caches, pacman cache, journal and trash
    Clean,
    /// Orphaned packages and configs
    Orphans,
    /// Put removed configs back from a Backup
    Restore,
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

/// What the App needs from the screen of a module.
pub trait ModuleScreen {
    /// Moves background work along; called before each frame.
    fn tick(&mut self);
    /// Handles a key, returning whether it was used.
    fn on_key(&mut self, key: KeyEvent) -> bool;
    fn draw(&mut self, frame: &mut Frame, area: Rect);
    /// The module's keys, for the footer.
    fn hint(&self) -> &'static str;
    /// Whether the keys that leave a module (q, Esc, h, ←) leave it now.
    fn can_leave(&self) -> bool;
    /// Whether root commands wait for [`ModuleScreen::run_root`].
    fn needs_terminal(&self) -> bool;
    /// Runs the waiting root commands; `run_root` runs them with the TUI paused.
    fn run_root(&mut self, run_root: &mut RunRoot) -> io::Result<()>;
}

pub struct App {
    /// The open module, or `None` for the menu.
    module: Option<Module>,
    /// Whether vole was opened on the menu; leaving a module then goes back instead of quitting.
    started_on_menu: bool,
    menu: ListState,
    config: Config,
    /// The `clean` screen once `clean` has been opened. It is kept when leaving, so opening
    /// `clean` again shows the same Plan instead of starting another scan.
    clean: Option<clean::Screen>,
    /// The `orphans` screen once `orphans` has been opened, kept like `clean`'s.
    orphans: Option<orphans::Screen>,
    /// The `restore` screen while `restore` is open; it lists the Backups again each time.
    restore: Option<orphans::RestoreScreen>,
    /// The `analyze` screen once `analyze` has been opened, kept like `clean`'s.
    analyze: Option<analyze::Screen>,
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
            orphans: None,
            restore: None,
            analyze: None,
            quit: false,
        };
        app.open(module);
        app
    }

    /// Opens `module`, or the menu for `None`.
    fn open(&mut self, module: Option<Module>) {
        self.module = module;
        match module {
            Some(Module::Clean) => {
                self.clean
                    .get_or_insert_with(|| clean::Screen::new(&self.config));
            }
            Some(Module::Orphans) => {
                self.orphans
                    .get_or_insert_with(|| orphans::Screen::new(&self.config));
            }
            // Backups change as orphans moves configs: listed afresh each time.
            Some(Module::Restore) => {
                self.restore = Some(orphans::RestoreScreen::new(&self.config));
            }
            Some(Module::Analyze) => {
                self.analyze
                    .get_or_insert_with(|| analyze::Screen::new(&self.config));
            }
            Some(Module::Status) | None => {}
        }
    }

    /// The screen of `module`, once it has been opened.
    fn screen(&mut self, module: Module) -> Option<&mut dyn ModuleScreen> {
        match module {
            Module::Clean => self.clean.as_mut().map(|s| s as &mut dyn ModuleScreen),
            Module::Orphans => self.orphans.as_mut().map(|s| s as &mut dyn ModuleScreen),
            Module::Restore => self.restore.as_mut().map(|s| s as &mut dyn ModuleScreen),
            Module::Analyze => self.analyze.as_mut().map(|s| s as &mut dyn ModuleScreen),
            Module::Status => None,
        }
    }

    pub fn run(mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        while !self.quit {
            for &module in Module::all() {
                if let Some(screen) = self.screen(module) {
                    screen.tick();
                }
            }
            terminal.draw(|frame| self.draw(frame))?;
            // Root commands need the terminal, which only the App has. Only the module on
            // screen can be waiting for it: no module lets you leave while it confirms or
            // removes.
            if let Some(screen) = self.module.and_then(|module| self.screen(module))
                && screen.needs_terminal()
            {
                screen.run_root(&mut |commands| term::run_external(terminal, commands))?;
                continue;
            }
            // Wake up now and then, so background work (the scans) shows up.
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
                // navigation; only keys it doesn't use leave it, and only where it allows.
                let (used, can_leave) = match self.screen(module) {
                    Some(screen) => (screen.on_key(key), screen.can_leave()),
                    None => (false, true),
                };
                if !used && can_leave {
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
                let mut hints = Vec::new();
                // Whether q leaves the module here, or the module uses it (e.g. to go back).
                let mut can_leave = true;
                match self.screen(module) {
                    Some(screen) => {
                        screen.draw(frame, body);
                        hints.push(screen.hint());
                        can_leave = screen.can_leave();
                    }
                    None => frame.render_widget(
                        Paragraph::new(format!("{}\n\nComing soon.", module.description()))
                            .block(Block::bordered().title(format!(" {} ", module.name()))),
                        body,
                    ),
                }
                if can_leave {
                    hints.push(if self.started_on_menu {
                        "q/h back"
                    } else {
                        "q quit"
                    });
                }
                hints.retain(|hint| !hint.is_empty());
                frame.render_widget(Line::from(format!(" {}", hints.join(" · "))), footer);
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
        for module in Module::all() {
            let mut app = App::new(Some(*module), Config::default());
            press(&mut app, KeyCode::Char('q'));
            assert!(app.quit, "{module:?}");
        }
    }

    #[test]
    fn clean_leaves_on_q_and_esc_once_the_plan_is_shown() {
        for code in [KeyCode::Char('q'), KeyCode::Esc] {
            let mut app = App::new(None, Config::default());
            press(&mut app, KeyCode::Enter);
            app.clean = Some(clean::Screen::with_plan(Default::default()));
            press(&mut app, code);
            assert_eq!(app.module, None);
        }
    }

    fn footer(app: &mut App) -> String {
        let backend = ratatui::backend::TestBackend::new(80, 6);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();
        let buffer = terminal.backend().buffer();
        (0..80).map(|x| buffer[(x, 5)].symbol()).collect()
    }

    #[test]
    fn footer_offers_quit_only_where_q_quits() {
        let mut first = crate::clean::plan::Category::new("Test");
        let mut item = crate::clean::plan::Item::new(
            "a",
            crate::clean::plan::Kind::Regenerable("/a".into()),
            1,
        );
        item.selected = true;
        first.items = vec![item];
        let mut app = App::new(Some(Module::Clean), Config::default());
        app.clean = Some(clean::Screen::with_plan(crate::clean::plan::Plan {
            categories: vec![first],
        }));
        assert!(footer(&mut app).contains("q quit"));
        press(&mut app, KeyCode::Enter);
        let confirming = footer(&mut app);
        assert!(confirming.contains("n back") && !confirming.contains("q quit"));
    }

    #[test]
    fn orphans_confirmation_goes_back_on_q() {
        let mut package = orphans::plan::Package::new("a", 1);
        package.selected = true;
        let mut app = App::new(Some(Module::Orphans), Config::default());
        app.orphans = Some(orphans::Screen::with_plan(orphans::plan::Plan {
            packages: vec![package],
            ..Default::default()
        }));
        assert!(footer(&mut app).contains("q quit"));
        press(&mut app, KeyCode::Enter);
        let confirming = footer(&mut app);
        assert!(confirming.contains("n back") && !confirming.contains("q quit"));
        press(&mut app, KeyCode::Char('q'));
        assert!(!app.quit);
        assert!(app.orphans.as_ref().is_some_and(orphans::Screen::has_plan));
    }

    #[test]
    fn nothing_leaves_orphans_while_removing() {
        let mut package = orphans::plan::Package::new("a", 1);
        package.selected = true;
        let mut app = App::new(Some(Module::Orphans), Config::default());
        app.orphans = Some(orphans::Screen::with_plan(orphans::plan::Plan {
            packages: vec![package],
            ..Default::default()
        }));
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Char('y'));
        for code in [KeyCode::Char('q'), KeyCode::Esc, KeyCode::Left] {
            press(&mut app, code);
            assert!(!app.quit, "{code:?}");
        }
        assert!(!footer(&mut app).contains("q quit"));
    }

    #[test]
    fn reopening_restore_lists_the_backups_again() {
        let mut app = App::new(None, Config::default());
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Char('j'));
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.module, Some(Module::Restore));
        app.restore = Some(orphans::RestoreScreen::with_backups(Vec::new()));
        assert!(
            app.restore
                .as_ref()
                .is_some_and(orphans::RestoreScreen::has_list)
        );
        press(&mut app, KeyCode::Char('q'));
        press(&mut app, KeyCode::Enter);
        assert!(
            !app.restore
                .as_ref()
                .is_some_and(orphans::RestoreScreen::has_list)
        );
    }

    #[test]
    fn reopening_clean_keeps_its_screen() {
        let mut app = App::new(None, Config::default());
        press(&mut app, KeyCode::Enter);
        app.clean = Some(clean::Screen::with_plan(Default::default()));
        press(&mut app, KeyCode::Char('q'));
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.module, Some(Module::Clean));
        assert!(app.clean.as_ref().is_some_and(clean::Screen::has_plan));
    }

    #[test]
    fn every_module_has_a_name_and_description() {
        for module in Module::all() {
            assert!(!module.name().is_empty());
            assert!(!module.description().is_empty());
        }
    }
}
