//! Applying the Plan: removing the selected packages with `sudo pacman -Rns`, and writing each
//! one to the Operation Log.

use std::collections::{BTreeSet, HashSet};
use std::io;

use super::pacman::{self, PACMAN};
use super::plan::{Package, Plan};
use crate::log::{Log, Outcome};
use crate::term::{self, RunRoot};

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
pub fn remove(
    plan: &Plan,
    log: &mut Log,
    run_root: &mut RunRoot,
    pacman: &dyn Pacman,
) -> (Vec<Outcome>, io::Result<()>) {
    let packages: Vec<&Package> = plan.selected().collect();
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
    use crate::term::Ran;
    use crate::testdir::TestDir;
    use std::fs;
    use std::os::unix::process::ExitStatusExt;
    use std::path::PathBuf;
    use std::process::{Command, ExitStatus};

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
            fs::read_to_string(Self::log_path(&self.dir))
                .unwrap()
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect()
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
        let (outcomes, terminal) = remove(plan, &mut log, &mut run_root, pacman);
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
