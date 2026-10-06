//! Applying the Plan: moving the selected configs into a Backup, removing the selected packages
//! with `sudo pacman -Rns`, and writing each one to the Operation Log.

use std::collections::{BTreeSet, HashSet};
use std::fs;
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::SystemTime;

use super::backup::{BACKUPS, NewBackup, OldBackup};
use super::configs::{self, OrphanedConfig};
use super::pacman::{self, PACMAN};
use super::plan::{Package, Plan};
use crate::disk::{self, Mounts};
use crate::log::{Log, Outcome};
use crate::safety::Rules;
use crate::term::{self, RunRoot};

/// What the Operation Log calls a config moved into a Backup.
const CONFIG: &str = "orphaned config";

/// What the Operation Log calls an old Backup deleted.
const BACKUP: &str = "backup";

/// Moves the selected configs into a new Backup in `home`, then deletes the selected old
/// Backups, each after a last safety check, and writes each to `log`. The new Backup is made
/// with the first config that moves, so when none does, there is none. `progress` is told about
/// each before it starts; once `stop` is set, the rest are left alone.
#[allow(clippy::too_many_arguments)]
pub fn remove_home(
    plan: &Plan,
    home: &Path,
    rules: &Rules,
    mounts: &Mounts,
    log: &mut Log,
    now: SystemTime,
    progress: &mut dyn FnMut(usize, usize, &str),
    stop: &AtomicBool,
) -> Vec<Outcome> {
    let configs: Vec<&OrphanedConfig> = plan.selected_configs().collect();
    let backups: Vec<&OldBackup> = plan.selected_backups().collect();
    let total = configs.len() + backups.len();
    let mut backup = None;
    let mut outcomes = Vec::new();
    for (done, config) in configs.iter().enumerate() {
        if stop.load(Ordering::Relaxed) {
            outcomes.push(not_attempted(&config.label, config.size));
            continue;
        }
        progress(done, total, &config.label);
        let target = config.path.display().to_string();
        let (label, size) = (&config.label, config.size);
        outcomes.push(
            match move_config(config, home, rules, mounts, &mut backup, now) {
                Ok(Some(to)) => {
                    let to = to.display().to_string();
                    log.record_moved(label, CONFIG, size, &target, &to, Ok(()))
                }
                // Already gone: nothing left to keep, nor freed by vole.
                Ok(None) => already_gone(log.record(label, CONFIG, 0, &target, Ok(()))),
                Err(err) => log.record(label, CONFIG, size, &target, Err(err)),
            },
        );
    }
    for (done, old) in backups.iter().enumerate() {
        if stop.load(Ordering::Relaxed) {
            outcomes.push(not_attempted(&old.label, old.size));
            continue;
        }
        progress(configs.len() + done, total, &old.label);
        let target = old.path.display().to_string();
        outcomes.push(match delete_backup(&old.path, home, rules, mounts) {
            Ok(true) => log.record(&old.label, BACKUP, old.size, &target, Ok(())),
            Ok(false) => already_gone(log.record(&old.label, BACKUP, 0, &target, Ok(()))),
            Err(err) => log.record(&old.label, BACKUP, old.size, &target, Err(err)),
        });
    }
    outcomes
}

/// Says an Item was gone before vole got to it.
fn already_gone(mut outcome: Outcome) -> Outcome {
    outcome.label.push_str(" (already gone)");
    outcome
}

/// The Outcome of an Item vole didn't try to remove, so there is nothing to log.
pub fn not_attempted(label: &str, size: u64) -> Outcome {
    Outcome {
        label: label.to_string(),
        size,
        error: Some("stopped before removing it".into()),
        log_error: None,
        moved: false,
    }
}

/// Deletes an old Backup for good, once it is still one: a folder (not a symlink) directly in
/// the backups folder, which the path rules allow and which isn't a mount point. Returns whether
/// it was still there.
fn delete_backup(path: &Path, home: &Path, rules: &Rules, mounts: &Mounts) -> Result<bool, String> {
    let refused = |why: &str| Err(format!("refused: {} {why}", path.display()));
    if path.parent() != Some(home.join(BACKUPS).as_path()) {
        return refused("is not a Backup");
    }
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => {}
        Ok(_) => return refused("is not a folder"),
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(false),
        Err(err) => return Err(format!("{}: {err}", path.display())),
    }
    let real = rules
        .check(path)
        .map_err(|refusal| format!("refused: {} is {refusal}", path.display()))?;
    if mounts.contains(&real) {
        return refused("is a mount point");
    }
    disk::remove_all(&real, mounts)
        .map(|()| true)
        .map_err(|err| format!("{}: {err}", path.display()))
}

/// Moves one config into `backup`, creating the Backup first when there is none yet. Returns
/// where it went, or `None` when it was already gone.
fn move_config(
    config: &OrphanedConfig,
    home: &Path,
    rules: &Rules,
    mounts: &Mounts,
    backup: &mut Option<NewBackup>,
    now: SystemTime,
) -> Result<Option<PathBuf>, String> {
    let path = &config.path;
    let refused = |why: &str| Err(format!("refused: {} {why}", path.display()));
    if !configs::is_candidate_path(home, path) {
        return refused("is not where configs are looked for");
    }
    if home.join(BACKUPS).starts_with(path) {
        return refused("holds vole's Backups");
    }
    // ~/.config/vole, ~/.local/state/vole (the Operation Log), …
    if path.file_name().is_some_and(|name| name == "vole") {
        return refused("is vole's own");
    }
    let real = rules
        .check(path)
        .map_err(|refusal| format!("refused: {} is {refusal}", path.display()))?;
    if mounts.contains(&real) {
        return refused("is a mount point");
    }
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_symlink() => return refused("is a symlink now"),
        Ok(_) => {}
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(format!("{}: {err}", path.display())),
    }
    let Ok(relative) = path.strip_prefix(home) else {
        return refused("is outside the home folder");
    };
    let backup = match backup {
        Some(backup) => backup,
        None => backup.insert(
            NewBackup::create(home, now).map_err(|err| format!("creating a Backup: {err}"))?,
        ),
    };
    match backup.take(&real, relative, config.size) {
        Ok(to) => Ok(Some(to)),
        Err(err) if err.kind() == ErrorKind::CrossesDevices => {
            refused("is on another filesystem than the Backups, so it stays")
        }
        Err(err) => Err(format!("{}: {err}", path.display())),
    }
}

/// What removing asks pacman, as the user, so tests can stand in for it.
pub trait Pacman {
    /// What `pacman -Rns` would remove when given `names`.
    fn would_remove(&self, names: &[&str]) -> Result<BTreeSet<String>, String>;
    /// Every installed package.
    fn installed(&self) -> Result<HashSet<String>, String>;
}

/// The real pacman.
pub struct System;

impl Pacman for System {
    fn would_remove(&self, names: &[&str]) -> Result<BTreeSet<String>, String> {
        pacman::would_remove(names).map_err(|err| format!("{err:#}"))
    }

    fn installed(&self) -> Result<HashSet<String>, String> {
        pacman::installed_names().map_err(|err| format!("{err:#}"))
    }
}

/// Removes the selected packages with one `sudo pacman -Rns`, once pacman says it would remove
/// exactly those, and writes each package to `log`. Also returns whether the TUI could be
/// paused and brought back for it.
pub fn remove_packages(
    plan: &Plan,
    log: &mut Log,
    run_root: &mut RunRoot,
    pacman: &dyn Pacman,
) -> (Vec<Outcome>, io::Result<()>) {
    let packages: Vec<&Package> = plan.selected_packages().collect();
    if packages.is_empty() {
        return (Vec::new(), Ok(()));
    }
    let names: Vec<&str> = packages
        .iter()
        .map(|package| package.name.as_str())
        .collect();
    let mut command = term::as_root(PACMAN);
    command.args(["-Rns", "--"]).args(&names);
    let target = term::describe(&command);
    if let Err(err) = check(&names, pacman) {
        let outcomes = packages
            .iter()
            .map(|package| record(log, package, &target, Err(err.clone())))
            .collect();
        return (outcomes, Ok(()));
    }

    let ran = run_root(std::slice::from_mut(&mut command));
    let failed = ran.error(0, "sudo pacman -Rns");
    // What is still installed decides, whatever the exit status: the user may have declined
    // pacman's own question, or a failed transaction may have removed part of it.
    let installed = pacman.installed();
    let outcomes = packages
        .iter()
        .map(|package| {
            let removed = match &installed {
                Ok(installed) => !installed.contains(&package.name),
                Err(_) => failed.is_none(),
            };
            let result = match &failed {
                _ if removed => Ok(()),
                Some(err) => Err(err.clone()),
                None => Err("still installed after sudo pacman -Rns".into()),
            };
            record(log, package, &target, result)
        })
        .collect();
    (outcomes, ran.terminal)
}

/// The last check before running anything as root: the names can't be read as options, and
/// `pacman -Rns` would remove exactly them. Anything else would remove what the Plan doesn't
/// show, which only happens when packages changed since the scan.
fn check(names: &[&str], pacman: &dyn Pacman) -> Result<(), String> {
    if let Some(name) = names.iter().find(|name| !pacman::is_package_name(name)) {
        return Err(format!("refused: {name:?} is not a package name"));
    }
    let would = pacman
        .would_remove(names)
        .map_err(|err| format!("refused: {err}"))?;
    let selected: BTreeSet<String> = names.iter().map(|name| name.to_string()).collect();
    let extra: Vec<&str> = would.difference(&selected).map(String::as_str).collect();
    if !extra.is_empty() {
        return Err(format!(
            "refused: pacman would also remove {}",
            extra.join(", ")
        ));
    }
    let kept: Vec<&str> = selected.difference(&would).map(String::as_str).collect();
    if !kept.is_empty() {
        return Err(format!(
            "refused: pacman would not remove {}",
            kept.join(", ")
        ));
    }
    Ok(())
}

fn record(log: &mut Log, package: &Package, target: &str, result: Result<(), String>) -> Outcome {
    log.record(&package.name, "package", package.size, target, result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orphans::configs::Confidence;
    use crate::term::Ran;
    use crate::testdir::TestDir;
    use std::os::unix::process::ExitStatusExt;
    use std::process::{Command, ExitStatus};

    struct Home {
        dir: TestDir,
        rules: Rules,
        log: PathBuf,
    }

    fn home() -> Home {
        let dir = TestDir::new();
        let rules = Rules::new(dir.path(), configs::ROOTS, &[])
            .unwrap()
            .with_loose_dotfiles();
        Home {
            log: dir.path().join("log.jsonl"),
            dir,
            rules,
        }
    }

    /// Selects each of `paths` as an Orphaned Config and moves them.
    fn move_all(home: &Home, paths: Vec<PathBuf>, mounts: &Mounts) -> Vec<Outcome> {
        let configs = paths
            .into_iter()
            .map(|path| OrphanedConfig {
                label: path.display().to_string(),
                size: 10,
                confidence: Confidence::High,
                reason: String::new(),
                in_use: false,
                selected: true,
                path,
            })
            .collect();
        let plan = Plan {
            configs,
            ..Plan::default()
        };
        remove(home, &plan, mounts)
    }

    fn remove(home: &Home, plan: &Plan, mounts: &Mounts) -> Vec<Outcome> {
        let mut log = Log::open(&home.log, "orphans").unwrap();
        let stop = AtomicBool::new(false);
        remove_home(
            plan,
            home.dir.path(),
            &home.rules,
            mounts,
            &mut log,
            SystemTime::now(),
            &mut |_, _, _| {},
            &stop,
        )
    }

    fn backups(home: &Home) -> Vec<PathBuf> {
        let mut found: Vec<PathBuf> = fs::read_dir(home.dir.path().join(BACKUPS))
            .into_iter()
            .flatten()
            .map(|entry| entry.unwrap().path())
            .collect();
        found.sort();
        found
    }

    fn log_lines(path: &Path) -> Vec<serde_json::Value> {
        fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    #[test]
    fn moves_configs_into_one_backup_and_logs_where() {
        let home = home();
        let gone = home.dir.dir(".config/gone");
        home.dir.file(".config/gone/settings", 10);
        let rc = home.dir.file(".gonerc", 10);
        let outcomes = move_all(&home, vec![gone.clone(), rc.clone()], &Mounts::default());
        assert!(
            outcomes.iter().all(|o| o.error.is_none() && o.moved),
            "{outcomes:?}"
        );
        assert!(!gone.exists() && !rc.exists());
        let backups = backups(&home);
        assert_eq!(backups.len(), 1);
        assert!(backups[0].join(".config/gone/settings").exists());
        assert!(backups[0].join(".gonerc").exists());
        let log = log_lines(&home.log);
        assert_eq!(log.len(), 2);
        assert_eq!(log[0]["kind"], "orphaned config");
        assert_eq!(log[0]["target"], gone.display().to_string());
        assert_eq!(
            log[0]["moved_to"],
            backups[0].join(".config/gone").display().to_string()
        );
    }

    #[test]
    fn refuses_what_is_not_a_config_to_move() {
        let home = home();
        let old_backup = home.dir.dir(".local/share/vole/backups/old");
        let mounted = home.dir.dir(".config/mounted");
        let refused = vec![
            // Not directly in a root.
            home.dir.dir(".config/app/deep"),
            // Outside the roots.
            home.dir.dir("Documents"),
            // Sealed.
            home.dir.dir(".ssh"),
            // Holds the Backups.
            home.dir.path().join(".local/share/vole"),
            // vole's own.
            home.dir.dir(".config/vole"),
            home.dir.dir(".local/state/vole"),
            home.dir
                .symlink(".config/linked", &home.dir.dir("elsewhere")),
            mounted.clone(),
        ];
        let outcomes = move_all(&home, refused.clone(), &Mounts::from_paths([mounted]));
        for (outcome, path) in outcomes.iter().zip(&refused) {
            let error = outcome.error.as_deref().unwrap_or_default();
            assert!(error.starts_with("refused"), "{}: {error}", path.display());
            assert!(fs::symlink_metadata(path).is_ok());
        }
        // No Backup was made for nothing.
        assert_eq!(backups(&home), [old_backup]);
    }

    fn old_backup(path: PathBuf) -> OldBackup {
        OldBackup {
            label: path.display().to_string(),
            configs: 1,
            size: 10,
            selected: true,
            path,
        }
    }

    #[test]
    fn deletes_old_backups_but_nothing_else() {
        let home = home();
        let old = home
            .dir
            .dir(".local/share/vole/backups/2026-08-01T00-00-00Z");
        home.dir.file(
            ".local/share/vole/backups/2026-08-01T00-00-00Z/.config/gone/a",
            10,
        );
        let not_a_backup = home.dir.dir(".config/app");
        let linked = home
            .dir
            .symlink(".local/share/vole/backups/linked", &home.dir.dir("keep"));
        let plan = Plan {
            backups: vec![
                old_backup(old.clone()),
                old_backup(not_a_backup.clone()),
                old_backup(linked.clone()),
            ],
            ..Plan::default()
        };
        let outcomes = remove(&home, &plan, &Mounts::default());
        assert!(outcomes[0].error.is_none(), "{:?}", outcomes[0]);
        assert!(!old.exists());
        assert!(
            outcomes[1..]
                .iter()
                .all(|o| o.error.as_deref().unwrap().starts_with("refused"))
        );
        assert!(not_a_backup.exists() && linked.exists() && home.dir.path().join("keep").exists());
        assert_eq!(log_lines(&home.log)[0]["kind"], "backup");
    }

    #[test]
    fn stopping_leaves_the_rest_alone() {
        let home = home();
        let first = home.dir.dir(".config/first");
        let second = home.dir.dir(".config/second");
        let old = home
            .dir
            .dir(".local/share/vole/backups/2026-08-01T00-00-00Z");
        let mut plan = Plan {
            backups: vec![old_backup(old.clone())],
            ..Plan::default()
        };
        for path in [first.clone(), second.clone()] {
            plan.configs.push(OrphanedConfig {
                label: path.display().to_string(),
                size: 10,
                confidence: Confidence::High,
                reason: String::new(),
                in_use: false,
                selected: true,
                path,
            });
        }
        let mut log = Log::open(&home.log, "orphans").unwrap();
        let stop = AtomicBool::new(false);
        let mut seen = Vec::new();
        let mut progress = |done: usize, total: usize, _: &str| {
            seen.push((done, total));
            stop.store(true, Ordering::Relaxed);
        };
        let outcomes = remove_home(
            &plan,
            home.dir.path(),
            &home.rules,
            &Mounts::default(),
            &mut log,
            SystemTime::now(),
            &mut progress,
            &stop,
        );
        assert_eq!(seen, [(0, 3)]);
        assert!(!first.exists() && second.exists() && old.exists());
        let errors: Vec<bool> = outcomes.iter().map(|o| o.error.is_some()).collect();
        assert_eq!(errors, [false, true, true]);
        assert_eq!(log_lines(&home.log).len(), 1);
    }

    #[test]
    fn a_config_already_gone_counts_as_removed() {
        let home = home();
        home.dir.dir(".config");
        let gone = home.dir.path().join(".config/gone");
        let outcomes = move_all(&home, vec![gone], &Mounts::default());
        assert!(outcomes[0].error.is_none() && !outcomes[0].moved);
        // Nothing freed by vole.
        assert_eq!(outcomes[0].size, 0);
        assert!(outcomes[0].label.ends_with("(already gone)"));
        assert!(backups(&home).is_empty());
    }

    struct Fake {
        would_remove: Result<BTreeSet<String>, String>,
        installed: Result<HashSet<String>, String>,
    }

    impl Pacman for Fake {
        fn would_remove(&self, _: &[&str]) -> Result<BTreeSet<String>, String> {
            self.would_remove.clone()
        }

        fn installed(&self) -> Result<HashSet<String>, String> {
            self.installed.clone()
        }
    }

    fn set<T: FromIterator<String>>(names: &[&str]) -> T {
        names.iter().map(|name| name.to_string()).collect()
    }

    /// Packages a, b and c, with a and b selected.
    fn plan() -> Plan {
        let mut packages = vec![
            Package::new("a", 1),
            Package::new("b", 2),
            Package::new("c", 4),
        ];
        packages[0].selected = true;
        packages[1].selected = true;
        Plan {
            packages,
            ..Plan::default()
        }
    }

    struct Run {
        dir: TestDir,
        commands: Vec<String>,
        outcomes: Vec<Outcome>,
        terminal: io::Result<()>,
    }

    impl Run {
        fn log_path(dir: &TestDir) -> PathBuf {
            dir.path().join("log.jsonl")
        }

        fn log(&self) -> Vec<serde_json::Value> {
            log_lines(&Self::log_path(&self.dir))
        }

        fn errors(&self) -> Vec<Option<&str>> {
            self.outcomes.iter().map(|o| o.error.as_deref()).collect()
        }
    }

    /// Removes `plan` with `pacman` standing in for pacman, and `ran` for what the TUI pause
    /// returns.
    fn apply(plan: &Plan, pacman: &Fake, ran: impl FnOnce() -> Ran) -> Run {
        let dir = TestDir::new();
        let mut log = Log::open(&Run::log_path(&dir), "orphans").unwrap();
        let mut commands = Vec::new();
        let mut ran = Some(ran);
        let mut run_root = |run: &mut [Command]| {
            commands.extend(run.iter().map(term::describe));
            ran.take().unwrap()()
        };
        let (outcomes, terminal) = remove_packages(plan, &mut log, &mut run_root, pacman);
        Run {
            dir,
            commands,
            outcomes,
            terminal,
        }
    }

    fn exited(code: i32) -> impl FnOnce() -> Ran {
        move || Ran {
            statuses: vec![Ok(ExitStatus::from_raw(code << 8))],
            terminal: Ok(()),
        }
    }

    #[test]
    fn removes_the_selection_with_one_sudo_pacman_and_logs_each_package() {
        let pacman = Fake {
            would_remove: Ok(set(&["a", "b"])),
            installed: Ok(set(&["c"])),
        };
        let run = apply(&plan(), &pacman, exited(0));
        let command = "/usr/bin/sudo /usr/bin/pacman -Rns -- a b";
        assert_eq!(run.commands, [command]);
        assert_eq!(run.errors(), [None, None]);
        let log = run.log();
        assert_eq!(log.len(), 2);
        for (line, name) in log.iter().zip(["a", "b"]) {
            assert_eq!(line["module"], "orphans");
            assert_eq!(line["item"], name);
            assert_eq!(line["kind"], "package");
            assert_eq!(line["target"], command);
            assert_eq!(line["removed"], true);
        }
        assert_eq!(log[1]["size"], 2);
    }

    #[test]
    fn refuses_names_that_could_be_options() {
        let mut plan = plan();
        plan.packages[1].name = "--hookdir=/tmp".into();
        let pacman = Fake {
            would_remove: Ok(set(&["a", "--hookdir=/tmp"])),
            installed: Ok(set(&["a", "--hookdir=/tmp"])),
        };
        let run = apply(&plan, &pacman, || unreachable!());
        assert!(run.commands.is_empty());
        assert!(
            run.errors()
                .iter()
                .all(|e| e.unwrap().starts_with("refused"))
        );
        assert_eq!(run.log()[0]["removed"], false);
    }

    #[test]
    fn refuses_when_pacman_would_remove_something_else() {
        let pacman = Fake {
            would_remove: Ok(set(&["a", "b", "libfoo"])),
            installed: Ok(set(&["a", "b", "c", "libfoo"])),
        };
        let run = apply(&plan(), &pacman, || unreachable!());
        assert!(run.commands.is_empty());
        let error = run.errors()[0].unwrap();
        assert!(error.contains("would also remove libfoo"), "{error}");
    }

    #[test]
    fn refuses_when_pacman_would_not_remove_them() {
        let pacman = Fake {
            would_remove: Err("pacman: removing b breaks dependency 'b' required by x".into()),
            installed: Ok(set(&["a", "b", "c", "x"])),
        };
        let run = apply(&plan(), &pacman, || unreachable!());
        assert!(run.commands.is_empty());
        assert!(run.errors()[1].unwrap().contains("breaks dependency"));
    }

    #[test]
    fn what_is_still_installed_decides() {
        // Declined at pacman's question: nothing went.
        let pacman = Fake {
            would_remove: Ok(set(&["a", "b"])),
            installed: Ok(set(&["a", "b", "c"])),
        };
        let run = apply(&plan(), &pacman, exited(1));
        let failed = "sudo pacman -Rns failed (exit status: 1)";
        assert_eq!(run.errors(), [Some(failed), Some(failed)]);
        assert_eq!(run.log()[0]["removed"], false);

        // A transaction that failed half way.
        let pacman = Fake {
            would_remove: Ok(set(&["a", "b"])),
            installed: Ok(set(&["b", "c"])),
        };
        let run = apply(&plan(), &pacman, exited(1));
        assert_eq!(run.errors(), [None, Some(failed)]);
    }

    #[test]
    fn falls_back_on_the_exit_status_when_pacman_cannot_be_asked() {
        let pacman = Fake {
            would_remove: Ok(set(&["a", "b"])),
            installed: Err("pacman: could not open database".into()),
        };
        assert_eq!(apply(&plan(), &pacman, exited(0)).errors(), [None, None]);
        let errors = apply(&plan(), &pacman, exited(1)).outcomes;
        assert!(errors.iter().all(|o| o.error.is_some()));
    }

    #[test]
    fn keeps_the_results_when_the_tui_cannot_pause() {
        let pacman = Fake {
            would_remove: Ok(set(&["a", "b"])),
            installed: Ok(set(&["a", "b", "c"])),
        };
        let run = apply(&plan(), &pacman, || Ran {
            statuses: Vec::new(),
            terminal: Err(io::Error::other("no tty")),
        });
        assert!(run.terminal.is_err());
        let error = run.errors()[0].unwrap();
        assert!(error.contains("could not be paused"), "{error}");
        assert_eq!(run.log().len(), 2);
    }
}
