//! The `orphans` screens: looking for Orphaned Packages and Configs, the Plan with selection,
//! confirmation, and the result.

use std::collections::HashSet;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::SystemTime;

use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, List, ListItem, ListState, Paragraph};

use super::backup::{BACKUPS, OLD_DAYS, OldBackup};
use super::configs::{Confidence, OrphanedConfig, PackageOwned};
use super::plan::{Package, Plan};
use super::{build_plan, refresh_in_use, remove, rules};
use crate::app::{Action, ModuleScreen, Tick};
use crate::config::Config;
use crate::disk::Mounts;
use crate::log::{Log, Outcome};
use crate::pacman::{ConfigFile, FileState};
use crate::safety::Rules;
use crate::term::RunRoot;
use crate::{size, ui};

pub struct Screen {
    config: Config,
    state: State,
    /// What packages just removed left behind (each path with who left it), for the next scan to
    /// put first.
    left: Vec<(PathBuf, String)>,
}

enum State {
    /// The scan starts on the first tick, so creating the screen touches nothing.
    NotStarted,
    Scanning(Receiver<Result<Plan, String>>),
    Ready(PlanView),
    /// Asking before anything is removed; `in_use` is the extra step for In Use configs.
    Confirm {
        view: PlanView,
        in_use: bool,
        scroll: u16,
    },
    /// Confirmed: the removal starts on the next tick.
    Removing(Plan),
    /// The configs and old Backups are being removed on a worker thread.
    RemovingHome {
        plan: Plan,
        receiver: Receiver<Progress>,
        stop: Arc<AtomicBool>,
        now: String,
    },
    /// The configs and old Backups are done; `sudo pacman -Rns` waits for the App to lend the
    /// terminal.
    RootPending {
        plan: Plan,
        outcomes: Vec<Outcome>,
        log: Log,
    },
    Done {
        outcomes: Vec<Outcome>,
        /// The configs the packages just removed left behind.
        leftovers: Vec<PackageOwned>,
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
            left: Vec::new(),
        }
    }

    /// Starts building the Plan in the background; what packages just removed `left` behind
    /// comes first.
    fn start(config: Config, left: Vec<(PathBuf, String)>) -> Receiver<Result<Plan, String>> {
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let plan = match dirs::home_dir() {
                Some(home) => build_plan(&home, &config, &left).map_err(|err| format!("{err:#}")),
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
            left: Vec::new(),
        }
    }

    #[cfg(test)]
    pub fn has_plan(&self) -> bool {
        matches!(self.state, State::Ready(_))
    }

    #[cfg(test)]
    fn is_removing(&self) -> bool {
        matches!(self.state, State::Removing(_))
    }

    /// Starts removing what was confirmed. Nothing is removed without the Operation Log, nor
    /// configs and old Backups without fresh path rules and the mount points; packages wait for
    /// the terminal.
    fn remove(&self, plan: Plan) -> State {
        let log = match Log::open_default("orphans") {
            Ok(log) => log,
            Err(err) => return State::Failed(format!("Nothing was removed: {err}")),
        };
        // Packages alone need nothing more: pacman checks them itself.
        let home_items =
            plan.selected_configs().next().is_some() || plan.selected_backups().next().is_some();
        if !home_items {
            return State::RootPending {
                plan,
                outcomes: Vec::new(),
                log,
            };
        }
        let prepared = (|| -> Result<(PathBuf, Rules, Mounts), String> {
            let home = dirs::home_dir().ok_or("the home directory could not be found")?;
            let rules = rules(&home, &self.config)
                .map_err(|err| format!("resolving {}: {err}", home.display()))?;
            let mounts = Mounts::load().map_err(|err| {
                format!("reading the mount points from /proc/self/mountinfo: {err}")
            })?;
            Ok((home, rules, mounts))
        })();
        match prepared {
            Ok((home, rules, mounts)) => Self::start_removal(plan, home, rules, mounts, log),
            Err(err) => State::Failed(format!("Nothing was removed: {err}")),
        }
    }

    /// Removes the configs and old Backups on a worker thread, so the screen keeps drawing.
    fn start_removal(
        plan: Plan,
        home: PathBuf,
        rules: Rules,
        mounts: Mounts,
        mut log: Log,
    ) -> State {
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
                &home,
                &rules,
                &mounts,
                &mut log,
                SystemTime::now(),
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
}

impl ModuleScreen for Screen {
    /// Moves background work along: starts the scan and picks up its result, and moves the
    /// confirmed configs. Then asks for the terminal while `sudo pacman -Rns` waits for
    /// [`ModuleScreen::run_root`].
    fn tick(&mut self) -> Tick {
        let state = std::mem::replace(&mut self.state, State::NotStarted);
        self.state = match state {
            State::NotStarted => {
                let left = std::mem::take(&mut self.left);
                State::Scanning(Self::start(self.config.clone(), left))
            }
            State::Scanning(receiver) => match receiver.try_recv() {
                Ok(Ok(plan)) => State::Ready(PlanView::new(plan)),
                Ok(Err(err)) => State::Failed(format!("Could not look for orphans: {err}")),
                Err(TryRecvError::Empty) => State::Scanning(receiver),
                Err(TryRecvError::Disconnected) => {
                    State::Failed("the scan stopped unexpectedly".into())
                }
            },
            State::Removing(plan) => self.remove(plan),
            State::RemovingHome {
                plan,
                receiver,
                stop,
                mut now,
            } => {
                loop {
                    match receiver.try_recv() {
                        Ok(Progress::Item(label)) => now = label,
                        Ok(Progress::Finished(mut outcomes, log)) => {
                            let packages = plan.selected_packages();
                            if stop.load(Ordering::Relaxed) {
                                outcomes.extend(packages.map(|package| {
                                    remove::not_attempted(&package.name, package.size)
                                }));
                            } else if packages.count() > 0 {
                                break State::RootPending {
                                    plan,
                                    outcomes,
                                    log,
                                };
                            }
                            break State::Done {
                                outcomes,
                                leftovers: Vec::new(),
                                scroll: 0,
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
                }
            }
            state => state,
        };
        if matches!(self.state, State::RootPending { .. }) {
            Tick::RunRoot
        } else {
            Tick::Nothing
        }
    }

    fn on_key(&mut self, key: KeyEvent) -> Action {
        let back = matches!(
            key.code,
            KeyCode::Char('n' | 'q' | 'h') | KeyCode::Esc | KeyCode::Left
        );
        if let State::RemovingHome { stop, .. } = &self.state
            && key.code == KeyCode::Esc
        {
            stop.store(true, Ordering::Relaxed);
            return Action::Used;
        }
        let state = std::mem::replace(&mut self.state, State::NotStarted);
        let (state, used) = match state {
            State::Ready(mut view) if key.code == KeyCode::Enter => {
                if view.plan.any_selected() {
                    refresh_in_use(&mut view.plan, &self.config);
                    let confirm = State::Confirm {
                        view,
                        in_use: false,
                        scroll: 0,
                    };
                    (confirm, true)
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
                let any_in_use = view.plan.selected_configs().any(|config| config.in_use);
                if any_in_use && !in_use {
                    let confirm = State::Confirm {
                        view,
                        in_use: true,
                        scroll: 0,
                    };
                    (confirm, true)
                } else {
                    (State::Removing(view.plan), true)
                }
            }
            State::Confirm { view, .. } if back => (State::Ready(view), true),
            State::Confirm {
                view,
                in_use,
                scroll,
            } => {
                let scroll = ui::scrolled(scroll, key);
                let confirm = State::Confirm {
                    view,
                    in_use,
                    scroll,
                };
                (confirm, true)
            }
            state @ (State::Removing(_)
            | State::RemovingHome { .. }
            | State::RootPending { .. }) => (state, true),
            // Scanning again, with what the removed packages left behind first.
            State::Done { leftovers, .. } if key.code == KeyCode::Enter => {
                self.left = leftovers
                    .into_iter()
                    .map(|left| (left.path, left.packages.join(", ")))
                    .collect();
                (State::NotStarted, true)
            }
            State::Failed(_) if key.code == KeyCode::Enter => (State::NotStarted, true),
            State::Done {
                outcomes,
                leftovers,
                scroll,
            } => {
                let next = ui::scrolled(scroll, key);
                let done = State::Done {
                    outcomes,
                    leftovers,
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
            State::Ready(_) => "↑↓/jk move · space select · a select section · enter remove…",
            State::Confirm { in_use: false, .. } => "↑↓ scroll · y remove · n back",
            State::Confirm { in_use: true, .. } => "↑↓ scroll · Y remove anyway · n back",
            State::RemovingHome { .. } => "esc stop after this Item",
            State::Done { ref leftovers, .. } if !leftovers.is_empty() => {
                "↑↓ scroll · enter look at what they left"
            }
            State::Done { .. } => "↑↓ scroll · enter scan again",
            State::Failed(_) => "enter scan again",
            State::NotStarted
            | State::Scanning(_)
            | State::Removing(_)
            | State::RootPending { .. } => "",
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
            State::Confirm { .. }
            | State::Removing(_)
            | State::RemovingHome { .. }
            | State::RootPending { .. } => false,
        }
    }

    /// Removes the confirmed packages; `run_root` runs `sudo pacman -Rns` with the TUI paused.
    /// Fails when the TUI couldn't be brought back afterwards; the result is logged either way.
    fn run_root(&mut self, run_root: &mut RunRoot) -> io::Result<()> {
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
        let (packages, terminal) =
            remove::remove_packages(&plan, &mut log, run_root, &remove::System);
        let leftovers = left_behind(&plan, &packages);
        outcomes.extend(packages);
        self.state = State::Done {
            outcomes,
            leftovers,
            scroll: 0,
        };
        terminal
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) {
        let block = Block::bordered().title(" orphans ");
        match &mut self.state {
            State::NotStarted | State::Scanning(_) => frame.render_widget(
                Paragraph::new("Looking for orphaned packages and configs…").block(block),
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
            State::Done {
                outcomes,
                leftovers,
                scroll,
            } => draw_done(frame, area, block, outcomes, leftovers, scroll),
        }
    }
}

/// The configs only the packages that `removed` says went owned: what they left behind.
fn left_behind(plan: &Plan, removed: &[Outcome]) -> Vec<PackageOwned> {
    let gone: HashSet<&str> = removed
        .iter()
        .filter(|outcome| outcome.error.is_none())
        .map(|outcome| outcome.label.as_str())
        .collect();
    plan.package_owned
        .iter()
        .filter(|owned| owned.packages.iter().all(|p| gone.contains(p.as_str())))
        .cloned()
        .collect()
}

/// The result, and the configs the removed packages left behind.
fn draw_done(
    frame: &mut Frame,
    area: Rect,
    block: Block,
    outcomes: &[Outcome],
    leftovers: &[PackageOwned],
    scroll: &mut u16,
) {
    let width = usize::from(block.inner(area).width);
    let mut lines = ui::outcome_lines(outcomes, width);
    if !leftovers.is_empty() {
        lines.push(Line::default());
        lines.push(Line::styled(
            "The removed packages left these configs behind; enter looks at them:",
            Style::new().add_modifier(Modifier::BOLD),
        ));
        for left in leftovers {
            lines.push(Line::from(vec![
                Span::raw(format!("  {}", left.label)),
                Span::styled(
                    format!("  {}", left.packages.join(", ")),
                    Style::new().add_modifier(Modifier::DIM),
                ),
            ]));
        }
    }
    ui::draw_scrolled(frame, area, block, lines, scroll);
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
    let packages: Vec<&Package> = plan.selected_packages().collect();
    let configs: Vec<&OrphanedConfig> = plan.selected_configs().collect();
    let backups: Vec<&OldBackup> = plan.selected_backups().collect();
    let width = usize::from(block.inner(area).width);
    let mut lines = Vec::new();
    if in_use {
        let count = configs.iter().filter(|config| config.in_use).count();
        lines.push(Line::styled(
            format!("{count} of these configs are in use by running programs."),
            bold,
        ));
        lines.push(Line::from(
            "Moving them can make those programs misbehave. Press Y (shift+y) to move them anyway.",
        ));
    } else {
        lines.push(Line::styled(question(&packages, &configs, &backups), bold));
        if !packages.is_empty() {
            lines.push(Line::from(
                "vole pauses so sudo can ask for your password; pacman then asks once more.",
            ));
        }
        if packages
            .iter()
            .any(|package| !package.config_files.is_empty())
        {
            lines.push(Line::from(
                "Their config files go too, even changed ones: -n keeps no .pacsave copies.",
            ));
        }
        if !configs.is_empty() {
            lines.push(Line::from(format!(
                "Configs aren't deleted: they move into a dated Backup in ~/{BACKUPS}/, and vole restore puts them back."
            )));
        }
        if !backups.is_empty() {
            lines.push(Line::from("Old Backups are deleted for good."));
        }
    }
    lines.push(Line::default());
    for package in &packages {
        let name = vec![Span::raw(format!("  {}", package.name))];
        lines.push(ui::right_aligned(name, size::format(package.size), width));
        lines.extend(config_file_lines(&package.config_files, 4));
    }
    for config in &configs {
        let mut spans = vec![
            Span::raw(format!("  {}", config.label)),
            Span::styled(
                format!("  {}", config.confidence.name()),
                Style::new().add_modifier(Modifier::DIM),
            ),
        ];
        if config.in_use {
            spans.push(Span::styled("  in use", bold));
        }
        lines.push(ui::right_aligned(spans, size::format(config.size), width));
    }
    for backup in &backups {
        let label = vec![Span::raw(format!("  {}", backup.label))];
        lines.push(ui::right_aligned(label, size::format(backup.size), width));
    }
    ui::draw_scrolled(frame, area, block, lines, scroll);
}

/// What the confirmation asks: `Remove these 2 packages, freeing about 3 MiB, and move this
/// config into a Backup?`
fn question(packages: &[&Package], configs: &[&OrphanedConfig], backups: &[&OldBackup]) -> String {
    let freed = |sizes: &mut dyn Iterator<Item = u64>| size::format(sizes.sum());
    let mut parts = Vec::new();
    match packages.len() {
        0 => {}
        1 => parts.push(format!(
            "remove this package, freeing about {}",
            freed(&mut packages.iter().map(|p| p.size))
        )),
        count => parts.push(format!(
            "remove these {count} packages, freeing about {}",
            freed(&mut packages.iter().map(|p| p.size))
        )),
    }
    match configs.len() {
        0 => {}
        1 => parts.push("move this config into a Backup".into()),
        count => parts.push(format!("move these {count} configs into a Backup")),
    }
    match backups.len() {
        0 => {}
        1 => parts.push(format!(
            "delete this old Backup, freeing about {}",
            freed(&mut backups.iter().map(|b| b.size))
        )),
        count => parts.push(format!(
            "delete these {count} old Backups, freeing about {}",
            freed(&mut backups.iter().map(|b| b.size))
        )),
    }
    let mut text = match parts.split_last() {
        None => return String::new(),
        Some((last, [])) => last.clone(),
        Some((last, rest)) => format!("{}, and {last}", rest.join(", ")),
    };
    text[..1].make_ascii_uppercase();
    text.push('?');
    text
}

/// A row of the Plan list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Row {
    /// The heading of the Orphaned Packages.
    Packages,
    Package(usize),
    /// The heading of the Orphaned Configs.
    Configs,
    Config(usize),
    /// The heading of the old Backups, shown only when there are some.
    Backups,
    Backup(usize),
}

/// The Plan as a list: the Orphaned Packages under their heading, then the Orphaned Configs, then
/// any old Backups.
struct PlanView {
    plan: Plan,
    rows: Vec<Row>,
    list: ListState,
}

impl PlanView {
    fn new(plan: Plan) -> Self {
        let mut rows = vec![Row::Packages];
        rows.extend((0..plan.packages.len()).map(Row::Package));
        rows.push(Row::Configs);
        rows.extend((0..plan.configs.len()).map(Row::Config));
        if !plan.backups.is_empty() {
            rows.push(Row::Backups);
            rows.extend((0..plan.backups.len()).map(Row::Backup));
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
                Some(Row::Package(package)) => self.plan.toggle_package(package),
                Some(Row::Packages) => self.plan.toggle_all_packages(),
                Some(Row::Config(config)) => self.plan.toggle_config(config),
                Some(Row::Configs) => self.plan.toggle_all_configs(),
                Some(Row::Backup(backup)) => self.plan.toggle_backup(backup),
                Some(Row::Backups) => self.plan.toggle_all_backups(),
                None => {}
            },
            KeyCode::Char('a') => match self.current() {
                Some(Row::Packages | Row::Package(_)) => self.plan.toggle_all_packages(),
                Some(Row::Configs | Row::Config(_)) => self.plan.toggle_all_configs(),
                Some(Row::Backups | Row::Backup(_)) => self.plan.toggle_all_backups(),
                None => {}
            },
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
        let items: Vec<ListItem> = self
            .rows
            .iter()
            .map(|&row| match row {
                Row::Packages => ListItem::new(self.packages_heading(width)),
                Row::Package(i) => self.item(i, width, height),
                Row::Configs => ListItem::new(self.configs_heading(width)),
                Row::Config(i) => ListItem::new(self.config_line(i, width)),
                Row::Backups => ListItem::new(self.backups_heading(width)),
                Row::Backup(i) => ListItem::new(self.backup_line(i, width)),
            })
            .collect();
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

    fn packages_heading(&self, width: usize) -> Line<'static> {
        let spans = vec![Span::styled(
            "Orphaned packages",
            Style::new().add_modifier(Modifier::BOLD),
        )];
        if self.plan.packages.is_empty() {
            return heading_empty(spans);
        }
        ui::right_aligned(spans, size::format(self.plan.packages_size()), width)
    }

    fn configs_heading(&self, width: usize) -> Line<'static> {
        let mut spans = vec![Span::styled(
            "Orphaned configs",
            Style::new().add_modifier(Modifier::BOLD),
        )];
        if let Some(err) = &self.plan.configs_error {
            spans.push(Span::styled(
                format!("  couldn't look for them: {err}"),
                Style::new().add_modifier(Modifier::BOLD),
            ));
            return Line::from(spans);
        }
        if let Some(note) = &self.plan.configs_note {
            spans.push(Span::styled(
                format!("  {note}"),
                Style::new().add_modifier(Modifier::DIM),
            ));
        }
        if self.plan.configs.is_empty() {
            return heading_empty(spans);
        }
        let total = self.plan.configs.iter().map(|config| config.size).sum();
        ui::right_aligned(spans, size::format(total), width)
    }

    fn backups_heading(&self, width: usize) -> Line<'static> {
        let spans = vec![
            Span::styled("Old Backups", Style::new().add_modifier(Modifier::BOLD)),
            Span::styled(
                format!("  {OLD_DAYS} days old or more"),
                Style::new().add_modifier(Modifier::DIM),
            ),
        ];
        let total = self.plan.backups.iter().map(|backup| backup.size).sum();
        ui::right_aligned(spans, size::format(total), width)
    }

    fn backup_line(&self, index: usize, width: usize) -> Line<'static> {
        let backup = &self.plan.backups[index];
        let check = if backup.selected { "[x]" } else { "[ ]" };
        let configs = match backup.configs {
            1 => "1 config".to_string(),
            count => format!("{count} configs"),
        };
        let spans = vec![
            Span::raw(format!("  {check} {}", backup.label)),
            Span::styled(
                format!(" · {configs}"),
                Style::new().add_modifier(Modifier::DIM),
            ),
        ];
        ui::right_aligned(spans, size::format(backup.size), width)
    }

    fn config_line(&self, index: usize, width: usize) -> Line<'static> {
        let config = &self.plan.configs[index];
        let mut confidence = Style::new();
        if config.confidence == Confidence::High {
            confidence = confidence.add_modifier(Modifier::BOLD);
        }
        let check = if config.selected { "[x]" } else { "[ ]" };
        let mut spans = vec![
            Span::raw(format!("  {check} {}", config.label)),
            Span::styled(format!("  {}", config.confidence.name()), confidence),
            Span::styled(
                format!(" · {}", config.reason),
                Style::new().add_modifier(Modifier::DIM),
            ),
        ];
        if config.in_use {
            spans.push(Span::styled(
                "  in use",
                Style::new().add_modifier(Modifier::BOLD),
            ));
        }
        ui::right_aligned(spans, size::format(config.size), width)
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

/// A heading with nothing under it.
fn heading_empty(mut spans: Vec<Span<'static>>) -> Line<'static> {
    spans.push(Span::styled(
        "  none found",
        Style::new().add_modifier(Modifier::DIM),
    ));
    Line::from(spans)
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
    use crate::orphans::configs::OrphanedConfig;
    use crate::orphans::plan::Package;

    fn screen() -> Screen {
        Screen::with_plan(Plan {
            packages: vec![Package::new("a", 10), Package::new("b", 20)],
            ..Plan::default()
        })
    }

    fn selected_size(screen: &Screen) -> u64 {
        match &screen.state {
            State::Ready(view) => view.plan.selected_size(),
            _ => panic!("no plan"),
        }
    }

    fn press(screen: &mut Screen, code: KeyCode) -> bool {
        screen.on_key(KeyEvent::from(code)) == Action::Used
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
        // The last row is the configs' heading, under the last package.
        press(&mut screen, KeyCode::Char(' '));
        assert_eq!(selected_size(&screen), 0);
        press(&mut screen, KeyCode::Up);
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
            ..Plan::default()
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
            ..Plan::default()
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
    fn lists_the_orphaned_configs_with_their_confidence() {
        let config = |label: &str, confidence, reason: &str, size| OrphanedConfig {
            path: label.replace('~', "/home/me").into(),
            label: label.into(),
            size,
            confidence,
            reason: reason.into(),
            in_use: false,
            selected: false,
        };
        let mut screen = Screen::with_plan(Plan {
            configs: vec![
                config(
                    "~/.config/gone",
                    Confidence::High,
                    "unchanged since Mar 2025",
                    2048,
                ),
                config("~/.gonerc", Confidence::Low, "dotfile directly in ~", 10),
            ],
            configs_note: Some("2 path(s) left out by the safety rules".into()),
            ..Plan::default()
        });
        let rows = render(&mut screen, 90, 8);
        assert!(
            rows[1].contains("Orphaned packages  none found"),
            "{rows:#?}"
        );
        assert!(rows[2].contains("Orphaned configs  2 path(s) left out by the safety rules"));
        assert!(
            rows[2]
                .trim_end_matches('│')
                .trim_end()
                .ends_with("2.0 KiB")
        );
        assert!(rows[3].contains("[ ] ~/.config/gone  High · unchanged since Mar 2025"));
        assert!(rows[4].contains("[ ] ~/.gonerc  Low · dotfile directly in ~"));
        // Space on a config selects it, a on the configs selects them all.
        press(&mut screen, KeyCode::Char('j'));
        press(&mut screen, KeyCode::Char('j'));
        press(&mut screen, KeyCode::Char(' '));
        assert_eq!(selected_size(&screen), 2048);
        press(&mut screen, KeyCode::Char('a'));
        assert_eq!(selected_size(&screen), 2058);
        press(&mut screen, KeyCode::Char('a'));
        assert_eq!(selected_size(&screen), 0);
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
        assert!(screen.is_removing());
    }

    #[test]
    fn keys_do_nothing_while_removing() {
        let mut screen = screen();
        press(&mut screen, KeyCode::Char('a'));
        press(&mut screen, KeyCode::Enter);
        press(&mut screen, KeyCode::Char('y'));
        assert!(press(&mut screen, KeyCode::Char('q')));
        assert!(press(&mut screen, KeyCode::Esc));
        assert!(screen.is_removing() && !screen.can_leave());
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
            ..Plan::default()
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

    fn configs(in_use: bool) -> Screen {
        let config = |label: &str, size| OrphanedConfig {
            path: label.replace('~', "/nonexistent-home").into(),
            label: label.into(),
            size,
            confidence: Confidence::High,
            reason: String::new(),
            in_use,
            selected: true,
        };
        Screen::with_plan(Plan {
            configs: vec![
                config("~/.config/gone", 2048),
                config("~/.cache/gone", 1024),
            ],
            ..Plan::default()
        })
    }

    #[test]
    fn configs_move_into_a_backup_after_confirming() {
        let mut screen = configs(false);
        press(&mut screen, KeyCode::Enter);
        let rows = render(&mut screen, 90, 10).join("\n");
        assert!(
            rows.contains("Move these 2 configs into a Backup?"),
            "{rows}"
        );
        assert!(rows.contains("they move into a dated Backup in ~/.local/share/vole/backups/"));
        assert!(rows.contains("│  ~/.config/gone  High "));
        assert!(!rows.contains("sudo"));
        press(&mut screen, KeyCode::Char('y'));
        assert!(screen.is_removing());
    }

    #[test]
    fn in_use_configs_need_a_second_confirmation() {
        let mut screen = configs(true);
        press(&mut screen, KeyCode::Enter);
        press(&mut screen, KeyCode::Char('y'));
        let rows = render(&mut screen, 100, 10).join("\n");
        assert!(rows.contains("2 of these configs are in use"), "{rows}");
        // A second y (a double-tap, auto-repeat) doesn't pass the In Use step.
        press(&mut screen, KeyCode::Char('y'));
        assert!(!screen.is_removing());
        press(&mut screen, KeyCode::Char('Y'));
        assert!(screen.is_removing());
    }

    #[test]
    fn the_question_names_what_goes_and_what_moves() {
        let a = Package::new("a", 1024);
        let b = Package::new("b", 1024);
        let configs = configs(false);
        let State::Ready(view) = &configs.state else {
            panic!("no plan");
        };
        let one: Vec<&OrphanedConfig> = view.plan.configs.iter().take(1).collect();
        let old = OldBackup {
            path: "/home/me/.local/share/vole/backups/2026-08-01T00-00-00Z".into(),
            label: "Backup from 1 Aug 2026, 00:00 UTC".into(),
            configs: 2,
            size: 4096,
            selected: true,
        };
        assert_eq!(
            question(&[&a], &[], &[]),
            "Remove this package, freeing about 1.0 KiB?"
        );
        assert_eq!(question(&[], &one, &[]), "Move this config into a Backup?");
        assert_eq!(
            question(&[&a, &b], &one, &[]),
            "Remove these 2 packages, freeing about 2.0 KiB, and move this config into a Backup?"
        );
        assert_eq!(
            question(&[&a], &one, &[&old]),
            "Remove this package, freeing about 1.0 KiB, move this config into a Backup, \
             and delete this old Backup, freeing about 4.0 KiB?"
        );
    }

    #[test]
    fn old_backups_are_listed_and_selected_on_their_own() {
        let old = |day: u32, size| OldBackup {
            path: format!("/home/me/.local/share/vole/backups/2026-08-0{day}T00-00-00Z").into(),
            label: format!("Backup from {day} Aug 2026, 00:00 UTC"),
            configs: 2,
            size,
            selected: false,
        };
        let mut screen = Screen::with_plan(Plan {
            backups: vec![old(1, 4096), old(2, 1024)],
            ..Plan::default()
        });
        let rows = render(&mut screen, 90, 9);
        assert!(
            rows[3].contains("Old Backups  30 days old or more"),
            "{rows:#?}"
        );
        assert!(rows[4].contains("[ ] Backup from 1 Aug 2026, 00:00 UTC · 2 configs"));
        // Packages, configs, the backups' heading, then the first backup.
        for _ in 0..3 {
            press(&mut screen, KeyCode::Char('j'));
        }
        press(&mut screen, KeyCode::Char(' '));
        assert_eq!(selected_size(&screen), 4096);
        press(&mut screen, KeyCode::Char('a'));
        assert_eq!(selected_size(&screen), 5120);
        press(&mut screen, KeyCode::Enter);
        let rows = render(&mut screen, 90, 9).join("\n");
        assert!(
            rows.contains("Delete these 2 old Backups, freeing about 5.0 KiB?"),
            "{rows}"
        );
        assert!(rows.contains("Old Backups are deleted for good."));
    }

    #[test]
    fn offers_what_removed_packages_left_behind() {
        let owned = |path: &str, packages: &[&str]| PackageOwned {
            path: format!("/home/me/{path}").into(),
            label: format!("~/{path}"),
            packages: packages.iter().map(|p| p.to_string()).collect(),
        };
        let plan = Plan {
            package_owned: vec![
                owned(".config/htop", &["htop"]),
                owned(".config/both", &["htop", "kept"]),
                owned(".config/failed", &["failed"]),
            ],
            ..Plan::default()
        };
        let outcome = |name: &str, error: Option<&str>| Outcome {
            label: name.into(),
            size: 1,
            error: error.map(String::from),
            log_error: None,
            moved: None,
        };
        let removed = [outcome("htop", None), outcome("failed", Some("no"))];
        let leftovers = left_behind(&plan, &removed);
        assert_eq!(leftovers, [owned(".config/htop", &["htop"])]);

        let mut screen = Screen::with_plan(Plan::default());
        screen.state = State::Done {
            outcomes: removed.into(),
            leftovers,
            scroll: 0,
        };
        let rows = render(&mut screen, 90, 12).join("\n");
        assert!(rows.contains("left these configs behind"), "{rows}");
        assert!(rows.contains("~/.config/htop  htop"));
        assert_eq!(screen.hint(), "↑↓ scroll · enter look at what they left");
        // Enter scans again, with them first.
        press(&mut screen, KeyCode::Enter);
        assert_eq!(
            screen.left,
            [(PathBuf::from("/home/me/.config/htop"), "htop".to_string())]
        );
    }

    #[test]
    fn says_why_configs_couldnt_be_looked_for() {
        let mut screen = Screen::with_plan(Plan {
            packages: vec![Package::new("a", 10)],
            configs_error: Some("reading known-owners.toml: expected `=`".into()),
            ..Plan::default()
        });
        let rows = render(&mut screen, 100, 8).join("\n");
        assert!(rows.contains("[ ] a"), "{rows}");
        assert!(rows.contains(
            "Orphaned configs  couldn't look for them: reading known-owners.toml: expected `=`"
        ));
        assert!(!rows.contains("none found  couldn't"));
    }

    /// The screen as the removal thread finishes, for a Plan with `packages` selected.
    fn finishing(packages: bool, stopped: bool) -> (Screen, mpsc::Sender<Progress>) {
        let mut plan = Plan::default();
        if packages {
            let mut package = Package::new("a", 10);
            package.selected = true;
            plan.packages.push(package);
        }
        let (sender, receiver) = mpsc::channel();
        let mut screen = Screen::with_plan(Plan::default());
        screen.state = State::RemovingHome {
            plan,
            receiver,
            stop: Arc::new(AtomicBool::new(stopped)),
            now: String::new(),
        };
        (screen, sender)
    }

    fn log() -> Log {
        let dir = crate::testdir::TestDir::new();
        Log::open(&dir.path().join("log.jsonl"), "orphans").unwrap()
    }

    #[test]
    fn shows_what_is_being_removed_and_stops_on_esc() {
        let (mut screen, sender) = finishing(false, false);
        sender
            .send(Progress::Item("~/.config/a (1 of 2)".into()))
            .unwrap();
        screen.tick();
        assert!(
            render(&mut screen, 60, 5)
                .join("")
                .contains("Removing ~/.config/a (1 of 2)…")
        );
        assert_eq!(screen.hint(), "esc stop after this Item");
        assert!(press(&mut screen, KeyCode::Esc));
        assert!(!screen.can_leave());
        assert!(
            render(&mut screen, 60, 5)
                .join("")
                .contains("Stopping after ~/.config/a")
        );
    }

    #[test]
    fn packages_wait_for_the_terminal_after_the_rest() {
        let (mut screen, sender) = finishing(true, false);
        sender.send(Progress::Finished(Vec::new(), log())).unwrap();
        assert_eq!(screen.tick(), Tick::RunRoot);
        // Until it has the terminal, it keeps asking.
        assert_eq!(screen.tick(), Tick::RunRoot);

        // Without packages, it is done.
        let (mut screen, sender) = finishing(false, false);
        sender.send(Progress::Finished(Vec::new(), log())).unwrap();
        assert_eq!(screen.tick(), Tick::Nothing);
        assert!(screen.can_leave());
    }

    #[test]
    fn stopping_leaves_the_packages_alone() {
        let (mut screen, sender) = finishing(true, true);
        sender.send(Progress::Finished(Vec::new(), log())).unwrap();
        assert_eq!(screen.tick(), Tick::Nothing);
        let State::Done { outcomes, .. } = &screen.state else {
            panic!("not done");
        };
        assert_eq!(outcomes[0].label, "a");
        assert_eq!(
            outcomes[0].error.as_deref(),
            Some("stopped before removing it")
        );
    }

    #[test]
    fn leaving_keys_are_not_used() {
        let mut screen = screen();
        assert!(!press(&mut screen, KeyCode::Char('q')));
        assert!(!press(&mut screen, KeyCode::Esc));
        assert!(screen.can_leave());
    }
}
