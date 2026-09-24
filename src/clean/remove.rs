//! Applying the Plan: removing the selected Items and writing the Operation Log.

use std::fs::{self, File, OpenOptions, Permissions};
use std::io::{self, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use super::plan::{Item, Kind, Plan};
use super::scan::{Mounts, trash_parts};
use crate::safety::Rules;

const SUDO: &str = "/usr/bin/sudo";

/// What happened to one selected Item.
#[derive(Debug)]
pub struct Outcome {
    pub label: String,
    /// Bytes freed; for root commands, the Plan's estimate.
    pub size: u64,
    pub error: Option<String>,
}

/// Runs root commands, all under one pause of the TUI.
pub type RunRoot<'a> = dyn FnMut(&mut [Command]) -> io::Result<Vec<io::Result<ExitStatus>>> + 'a;

/// Removes every selected Item: home Items directly, each after a last safety check, then the
/// root commands through `sudo`. Every attempt is written to `log`.
pub fn apply(
    plan: &Plan,
    rules: &Rules,
    mounts: &Mounts,
    log: &mut Log,
    run_root: &mut RunRoot,
) -> Vec<Outcome> {
    let mut outcomes = Vec::new();
    let mut root = Vec::new();
    for item in plan.categories.iter().flat_map(|c| &c.items) {
        if !item.selected {
            continue;
        }
        let result = match &item.kind {
            Kind::Regenerable(path) => remove_regenerable(path, rules, mounts),
            Kind::Trash(path) => empty_trash(path, rules, mounts),
            Kind::Root(command) => {
                root.push((*command, item));
                continue;
            }
        };
        let target = item.path().unwrap_or(Path::new("")).display().to_string();
        outcomes.push(log.record(item, &target, result));
    }

    root.sort_by_key(|(command, _)| command.order());
    let mut commands: Vec<Command> = root
        .iter()
        .map(|(command, _)| {
            let mut sudo = Command::new(SUDO);
            sudo.arg(command.program()).args(command.args());
            sudo
        })
        .collect();
    if commands.is_empty() {
        return outcomes;
    }
    let targets: Vec<String> = commands.iter().map(crate::term::describe).collect();
    let mut statuses = match run_root(&mut commands) {
        Ok(statuses) => statuses.into_iter(),
        Err(err) => {
            let err = format!("the terminal could not be paused: {err}");
            for ((_, item), target) in root.iter().zip(&targets) {
                outcomes.push(log.record(item, target, Err(err.clone())));
            }
            return outcomes;
        }
    };
    for ((_, item), target) in root.iter().zip(&targets) {
        let result = match statuses.next() {
            Some(Ok(status)) if status.success() => Ok(()),
            Some(Ok(status)) => Err(format!("{target} failed ({status})")),
            Some(Err(err)) => Err(format!("{target} could not run: {err}")),
            None => Err(format!("{target} did not run")),
        };
        outcomes.push(log.record(item, target, result));
    }
    outcomes
}

/// The last check before removing `path`: the path rules, and not a mount point.
fn final_check(path: &Path, rules: &Rules, mounts: &Mounts) -> Result<PathBuf, String> {
    rules
        .check(path)
        .map_err(|refusal| format!("refused: {} is {refusal}", path.display()))?;
    let real = rules
        .resolve(path)
        .ok_or_else(|| format!("refused: {} moved", path.display()))?;
    if mounts.contains(&real) {
        return Err(format!("refused: {} is a mount point", path.display()));
    }
    Ok(real)
}

fn remove_regenerable(path: &Path, rules: &Rules, mounts: &Mounts) -> Result<(), String> {
    let real = final_check(path, rules, mounts)?;
    remove(&real, mounts).map_err(|err| format!("{}: {err}", path.display()))
}

/// Removes what is inside the trash, not the trash folder itself.
fn empty_trash(trash: &Path, rules: &Rules, mounts: &Mounts) -> Result<(), String> {
    final_check(trash, rules, mounts)?;
    if !fs::symlink_metadata(trash).is_ok_and(|meta| meta.is_dir()) {
        return Err(format!("refused: {} is not a folder", trash.display()));
    }
    let mut first_error = None;
    for part in trash_parts(trash, rules) {
        let entries = fs::read_dir(&part).map_err(|err| format!("{}: {err}", part.display()))?;
        for entry in entries {
            let result = entry
                .map_err(|err| err.to_string())
                .and_then(|entry| remove_regenerable(&entry.path(), rules, mounts));
            if let Err(err) = result {
                first_error.get_or_insert(err);
            }
        }
    }
    first_error.map_or(Ok(()), Err)
}

/// Removes `path` (a real path) without following symlinks and without entering other
/// filesystems or mount points. Read-only folders, like Go's module cache, are made writable.
fn remove(path: &Path, mounts: &Mounts) -> io::Result<()> {
    let meta = fs::symlink_metadata(path)?;
    remove_tree(path, &meta, meta.dev(), mounts)
}

fn remove_tree(path: &Path, meta: &fs::Metadata, device: u64, mounts: &Mounts) -> io::Result<()> {
    if !meta.is_dir() {
        return fs::remove_file(path);
    }
    if meta.dev() != device || mounts.contains(path) {
        return Err(io::Error::other(format!(
            "{} is a mount point",
            path.display()
        )));
    }
    let mode = meta.permissions().mode();
    if mode & 0o700 != 0o700 {
        fs::set_permissions(path, Permissions::from_mode(mode | 0o700))?;
    }
    let mut first_error = None;
    for entry in fs::read_dir(path)? {
        let result = entry.and_then(|entry| {
            let meta = entry.metadata()?;
            remove_tree(&entry.path(), &meta, device, mounts)
        });
        if let Err(err) = result {
            first_error.get_or_insert(err);
        }
    }
    match first_error {
        Some(err) => Err(err),
        None => fs::remove_dir(path),
    }
}

/// The Operation Log: one JSON line per Item vole tried to remove, and the outcome.
pub struct Log {
    file: File,
}

#[derive(Serialize)]
struct Entry<'a> {
    /// Seconds since the Unix epoch.
    time: u64,
    module: &'static str,
    item: &'a str,
    /// The removed path, or the command that was run.
    target: &'a str,
    kind: &'static str,
    size: u64,
    removed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<&'a str>,
}

impl Log {
    pub fn path() -> Option<PathBuf> {
        dirs::state_dir().map(|dir| dir.join("vole").join("log.jsonl"))
    }

    pub fn open(path: &Path) -> io::Result<Self> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Self { file })
    }

    /// Writes the attempt to the log and turns it into an Outcome.
    fn record(&mut self, item: &Item, target: &str, result: Result<(), String>) -> Outcome {
        let entry = Entry {
            time: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_secs()),
            module: "clean",
            item: &item.label,
            target,
            kind: match item.kind {
                Kind::Regenerable(_) => "regenerable",
                Kind::Trash(_) => "trash",
                Kind::Root(_) => "root",
            },
            size: item.size,
            removed: result.is_ok(),
            error: result.as_ref().err().map(String::as_str),
        };
        let written = self.write(&entry);
        let mut error = result.err();
        if let Err(err) = written {
            let note = format!("not written to the operation log: {err}");
            error = Some(match error {
                Some(error) => format!("{error}; {note}"),
                None => note,
            });
        }
        Outcome {
            label: item.label.clone(),
            size: item.size,
            error,
        }
    }

    fn write(&mut self, entry: &Entry) -> io::Result<()> {
        let mut line = serde_json::to_string(entry)?;
        line.push('\n');
        self.file.write_all(line.as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clean::plan::{Category, RootCommand};
    use crate::clean::scan::ALLOWED_ROOTS;
    use crate::testdir::TestDir;
    use std::os::unix::process::ExitStatusExt;

    struct Setup {
        home: TestDir,
        rules: Rules,
        log_path: PathBuf,
    }

    fn setup(home: TestDir) -> Setup {
        let rules = Rules::new(home.path(), ALLOWED_ROOTS, &[]).unwrap();
        let log_path = home.path().join(".local/state/vole/log.jsonl");
        Setup {
            home,
            rules,
            log_path,
        }
    }

    fn selected(kind: Kind) -> Item {
        let mut item = Item::new("item", kind, 1);
        item.selected = true;
        item
    }

    fn apply_items(setup: &Setup, items: Vec<Item>, mounts: &Mounts) -> Vec<Outcome> {
        let mut category = Category::new("Test");
        category.items = items;
        let plan = Plan {
            categories: vec![category],
        };
        let mut log = Log::open(&setup.log_path).unwrap();
        let mut no_root = |_: &mut [Command]| -> io::Result<Vec<io::Result<ExitStatus>>> {
            panic!("no root command expected")
        };
        apply(&plan, &setup.rules, mounts, &mut log, &mut no_root)
    }

    fn log_lines(setup: &Setup) -> Vec<serde_json::Value> {
        fs::read_to_string(&setup.log_path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    #[test]
    fn removes_selected_caches_and_logs_them() {
        let s = setup(TestDir::new());
        let cache = s.home.dir(".cache/app");
        s.home.file(".cache/app/sub/a", 10);
        let kept = s.home.file(".cache/other/a", 10);
        let mut unselected = Item::new("other", Kind::Regenerable(kept.clone()), 1);
        unselected.selected = false;

        let outcomes = apply_items(
            &s,
            vec![selected(Kind::Regenerable(cache.clone())), unselected],
            &Mounts::default(),
        );
        assert_eq!(outcomes.len(), 1);
        assert!(outcomes[0].error.is_none(), "{:?}", outcomes[0]);
        assert!(!cache.exists());
        assert!(kept.exists());

        let log = log_lines(&s);
        assert_eq!(log.len(), 1);
        assert_eq!(log[0]["removed"], true);
        assert_eq!(log[0]["kind"], "regenerable");
        assert_eq!(log[0]["target"], cache.display().to_string());
    }

    #[test]
    fn removes_read_only_folders() {
        let s = setup(TestDir::new());
        let module = s.home.dir("go/pkg/mod/example.com/m@v1");
        s.home.file("go/pkg/mod/example.com/m@v1/go.mod", 10);
        fs::set_permissions(&module, Permissions::from_mode(0o555)).unwrap();
        let root = s.home.path().join("go/pkg/mod");
        let outcomes = apply_items(
            &s,
            vec![selected(Kind::Regenerable(root.clone()))],
            &Mounts::default(),
        );
        assert!(outcomes[0].error.is_none(), "{:?}", outcomes[0]);
        assert!(!root.exists());
    }

    #[test]
    fn refuses_a_path_outside_the_roots() {
        let s = setup(TestDir::new());
        let documents = s.home.dir("Documents");
        let outcomes = apply_items(
            &s,
            vec![selected(Kind::Regenerable(documents.clone()))],
            &Mounts::default(),
        );
        assert!(outcomes[0].error.as_ref().unwrap().contains("refused"));
        assert!(documents.exists());
        assert_eq!(log_lines(&s)[0]["removed"], false);
    }

    #[test]
    fn refuses_a_path_that_became_protected_or_sealed() {
        let home = TestDir::new();
        let cache = home.dir(".cache/app");
        let keys = home.dir(".cache/keys");
        // The folder the Plan proposed has since become the target of ~/.ssh.
        home.symlink(".ssh", &keys);
        let rules = Rules::new(home.path(), ALLOWED_ROOTS, std::slice::from_ref(&cache)).unwrap();
        let s = Setup {
            log_path: home.path().join("log.jsonl"),
            home,
            rules,
        };
        let outcomes = apply_items(
            &s,
            vec![
                selected(Kind::Regenerable(cache.clone())),
                selected(Kind::Regenerable(keys.clone())),
            ],
            &Mounts::default(),
        );
        assert!(outcomes.iter().all(|o| o.error.is_some()));
        assert!(cache.exists() && keys.exists());
    }

    #[test]
    fn does_not_follow_a_symlink_out_of_the_root() {
        let s = setup(TestDir::new());
        let outside = TestDir::new();
        let precious = outside.file("precious", 10);
        let link = s.home.symlink(".cache/link", outside.path());
        // Through the link: refused.
        let through = s.home.path().join(".cache/link/precious");
        let outcomes = apply_items(
            &s,
            vec![selected(Kind::Regenerable(through))],
            &Mounts::default(),
        );
        assert!(outcomes[0].error.is_some());
        // The link itself: only the link goes.
        let outcomes = apply_items(
            &s,
            vec![selected(Kind::Regenerable(link.clone()))],
            &Mounts::default(),
        );
        assert!(outcomes[0].error.is_none());
        assert!(fs::symlink_metadata(&link).is_err());
        assert!(precious.exists());
    }

    #[test]
    fn does_not_enter_mount_points() {
        let s = setup(TestDir::new());
        let cache = s.home.dir(".cache/app");
        let mounted = s.home.file(".cache/app/bind/project", 10);
        let mounts = Mounts::from_paths([mounted.parent().unwrap().to_path_buf()]);
        let outcomes = apply_items(
            &s,
            vec![selected(Kind::Regenerable(cache.clone()))],
            &mounts,
        );
        assert!(outcomes[0].error.is_some());
        assert!(mounted.exists());
    }

    #[test]
    fn empties_the_trash_but_keeps_the_folder() {
        let s = setup(TestDir::new());
        s.home.file(".local/share/Trash/files/old.txt", 10);
        s.home.file(".local/share/Trash/files/dir/a", 10);
        s.home.file(".local/share/Trash/info/old.txt.trashinfo", 10);
        let trash = s.home.path().join(".local/share/Trash");
        let outcomes = apply_items(
            &s,
            vec![selected(Kind::Trash(trash.clone()))],
            &Mounts::default(),
        );
        assert!(outcomes[0].error.is_none(), "{:?}", outcomes[0]);
        assert_eq!(fs::read_dir(trash.join("files")).unwrap().count(), 0);
        assert_eq!(fs::read_dir(trash.join("info")).unwrap().count(), 0);
        assert!(trash.join("files").is_dir());
    }

    #[test]
    fn empties_the_trash_without_following_symlinks() {
        let s = setup(TestDir::new());
        let keys = s.home.file(".ssh/id_ed25519", 10);
        s.home
            .symlink(".local/share/Trash/files", keys.parent().unwrap());
        s.home.file(".local/share/Trash/info/x.trashinfo", 10);
        let trash = s.home.path().join(".local/share/Trash");
        apply_items(&s, vec![selected(Kind::Trash(trash))], &Mounts::default());
        assert!(keys.exists());
    }

    #[test]
    fn runs_root_commands_through_sudo_in_order() {
        let s = setup(TestDir::new());
        let mut category = Category::new("Root");
        category.items = vec![
            selected(Kind::Root(RootCommand::JournalVacuum(200))),
            selected(Kind::Root(RootCommand::PacmanKeep(2))),
            selected(Kind::Root(RootCommand::PacmanUninstalled)),
        ];
        let plan = Plan {
            categories: vec![category],
        };
        let mut log = Log::open(&s.log_path).unwrap();
        let mut ran = Vec::new();
        let mut run = |commands: &mut [Command]| -> io::Result<Vec<io::Result<ExitStatus>>> {
            ran = commands.iter().map(crate::term::describe).collect();
            // The journal vacuum fails.
            Ok(vec![
                Ok(ExitStatus::from_raw(0)),
                Ok(ExitStatus::from_raw(0)),
                Ok(ExitStatus::from_raw(1 << 8)),
            ])
        };
        let outcomes = apply(&plan, &s.rules, &Mounts::default(), &mut log, &mut run);
        assert_eq!(
            ran,
            [
                "/usr/bin/sudo /usr/bin/paccache -r -u -k0",
                "/usr/bin/sudo /usr/bin/paccache -r -k2",
                "/usr/bin/sudo /usr/bin/journalctl --rotate --vacuum-size=200M",
            ]
        );
        let failed: Vec<bool> = outcomes.iter().map(|o| o.error.is_some()).collect();
        assert_eq!(failed, [false, false, true]);
        assert_eq!(log_lines(&s).len(), 3);
    }
}
