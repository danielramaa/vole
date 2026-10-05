//! Applying the Plan: removing the selected Items and writing the Operation Log.

use std::fs::{self, Permissions};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};

use super::plan::{Item, Kind, Plan, RootCommand};
use super::scan::{Mounts, is_real_dir, trash_parts};
use crate::log::{Log, Outcome};
use crate::safety::Rules;
use crate::term::Ran;

const SUDO: &str = "/usr/bin/sudo";

/// Runs root commands, all under one pause of the TUI.
pub type RunRoot<'a> = dyn FnMut(&mut [Command]) -> Ran + 'a;

/// Removes the selected home Items, each after a last safety check, and writes each attempt
/// to `log`. `progress` is told about each Item before it is removed. Once `stop` is set, the
/// Items not started yet are left alone.
pub fn remove_home(
    plan: &Plan,
    rules: &Rules,
    mounts: &Mounts,
    log: &mut Log,
    progress: &mut dyn FnMut(usize, usize, &str),
    stop: &AtomicBool,
) -> Vec<Outcome> {
    let items: Vec<&Item> = plan
        .selected()
        .filter(|item| item.path().is_some())
        .collect();
    let mut outcomes = Vec::new();
    for (done, item) in items.iter().enumerate() {
        if stop.load(Ordering::Relaxed) {
            outcomes.push(not_attempted(item, "stopped before removing it"));
            continue;
        }
        progress(done, items.len(), &item.label);
        let result = match &item.kind {
            Kind::Regenerable(path) => remove_regenerable(path, rules, mounts),
            Kind::Trash(path) => empty_trash(path, rules, mounts),
            Kind::Root(_) => continue,
        };
        let target = item.path().unwrap_or(Path::new("")).display().to_string();
        outcomes.push(record(log, item, &target, result));
    }
    outcomes
}

/// Runs the selected root commands through `sudo`, in order, and writes each to `log`. Also
/// returns whether the TUI could be paused and brought back for them.
pub fn remove_root(
    plan: &Plan,
    log: &mut Log,
    run_root: &mut RunRoot,
) -> (Vec<Outcome>, io::Result<()>) {
    let mut root: Vec<(RootCommand, &Item)> = plan
        .selected()
        .filter_map(|item| match item.kind {
            Kind::Root(command) => Some((command, item)),
            Kind::Regenerable(_) | Kind::Trash(_) => None,
        })
        .collect();
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
        return (Vec::new(), Ok(()));
    }
    let targets: Vec<String> = commands.iter().map(crate::term::describe).collect();
    let ran = run_root(&mut commands);
    let not_run = match &ran.terminal {
        Err(err) if ran.statuses.is_empty() => format!("the terminal could not be paused: {err}"),
        _ => "did not run".to_string(),
    };
    let mut statuses = ran.statuses.into_iter();
    let mut outcomes = Vec::new();
    for ((_, item), target) in root.iter().zip(&targets) {
        let result = match statuses.next() {
            Some(Ok(status)) if status.success() => Ok(()),
            Some(Ok(status)) => Err(format!("{target} failed ({status})")),
            Some(Err(err)) => Err(format!("{target} could not run: {err}")),
            None => Err(format!("{target} {not_run}")),
        };
        outcomes.push(record(log, item, target, result));
    }
    (outcomes, ran.terminal)
}

/// Writes the attempt to remove `item` to the Operation Log.
fn record(log: &mut Log, item: &Item, target: &str, result: Result<(), String>) -> Outcome {
    let kind = match item.kind {
        Kind::Regenerable(_) => "regenerable",
        Kind::Trash(_) => "trash",
        Kind::Root(_) => "root",
    };
    log.record(&item.label, kind, item.size, target, result)
}

/// The Outcome of an Item vole didn't try to remove, so there is nothing to log.
pub fn not_attempted(item: &Item, why: &str) -> Outcome {
    Outcome {
        label: item.label.clone(),
        size: item.size,
        error: Some(why.to_string()),
        log_error: None,
    }
}

/// The last check before removing `path`: the path rules, and not a mount point.
fn final_check(path: &Path, rules: &Rules, mounts: &Mounts) -> Result<PathBuf, String> {
    let real = rules
        .check(path)
        .map_err(|refusal| format!("refused: {} is {refusal}", path.display()))?;
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
    if !is_real_dir(trash) {
        return Err(format!("refused: {} is not a folder", trash.display()));
    }
    let trash_meta =
        fs::symlink_metadata(trash).map_err(|err| format!("{}: {err}", trash.display()))?;
    let mut first_error = None;
    let mut usable = Vec::new();
    for part in trash_parts(trash, rules) {
        // A part mounted from elsewhere is not the trash's to empty.
        let same_device = fs::symlink_metadata(&part).is_ok_and(|m| m.dev() == trash_meta.dev());
        let checked = final_check(&part, rules, mounts).and_then(|_| {
            same_device
                .then_some(())
                .ok_or_else(|| format!("refused: {} is a mount point", part.display()))
        });
        match checked {
            Ok(()) => usable.push(part),
            Err(err) => {
                first_error.get_or_insert(err);
            }
        }
    }
    let mut note = |result: Result<(), String>| {
        if let Err(err) = result {
            first_error.get_or_insert(err);
        }
    };
    let files = trash.join("files");
    let info = trash.join("info");
    let expunged = trash.join("expunged");

    // Each trashed file, then its .trashinfo, which is kept when the file couldn't go so the
    // desktop trash still lists it.
    if usable.contains(&files) {
        for entry in read_entries(&files, &mut note) {
            let removed = remove_regenerable(&entry, rules, mounts);
            if removed.is_ok() && usable.contains(&info) {
                let mut name = entry.file_name().unwrap_or_default().to_os_string();
                name.push(".trashinfo");
                note(remove_regenerable(&info.join(name), rules, mounts));
            }
            note(removed);
        }
        // .trashinfo files whose trashed file is already gone.
        if usable.contains(&info) {
            for entry in read_entries(&info, &mut note) {
                let name = entry.file_name().unwrap_or_default().as_bytes();
                let Some(stem) = name.strip_suffix(b".trashinfo") else {
                    continue;
                };
                let file = files.join(std::ffi::OsStr::from_bytes(stem));
                if fs::symlink_metadata(&file)
                    .is_err_and(|err| err.kind() == io::ErrorKind::NotFound)
                {
                    note(remove_regenerable(&entry, rules, mounts));
                }
            }
        }
    }
    if usable.contains(&expunged) {
        for entry in read_entries(&expunged, &mut note) {
            note(remove_regenerable(&entry, rules, mounts));
        }
    }
    first_error.map_or(Ok(()), Err)
}

/// The paths inside `dir`. Errors go to `note`, and the entries that could be read are still
/// returned.
fn read_entries(dir: &Path, note: &mut impl FnMut(Result<(), String>)) -> Vec<PathBuf> {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) => {
            note(Err(format!("{}: {err}", dir.display())));
            return Vec::new();
        }
    };
    let mut paths = Vec::new();
    for entry in entries {
        match entry {
            Ok(entry) => paths.push(entry.path()),
            Err(err) => note(Err(format!("{}: {err}", dir.display()))),
        }
    }
    paths
}

/// Removes `path` (a real path) without following symlinks and without entering other
/// filesystems or mount points. Read-only folders, like Go's module cache, are made writable.
/// Anything that disappears meanwhile (a program cleaning its own cache) counts as removed.
fn remove(path: &Path, mounts: &Mounts) -> io::Result<()> {
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(err) => return gone_is_fine(Err(err)),
    };
    // The folder above is the baseline, so an Item that is itself on another filesystem is
    // caught even without the mount points.
    let parent = path.parent().unwrap_or(path);
    let device = fs::symlink_metadata(parent)?.dev();
    remove_tree(path, &meta, device, mounts)
}

fn gone_is_fine(result: io::Result<()>) -> io::Result<()> {
    match result {
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

fn remove_tree(path: &Path, meta: &fs::Metadata, device: u64, mounts: &Mounts) -> io::Result<()> {
    if !meta.is_dir() {
        return gone_is_fine(fs::remove_file(path));
    }
    if meta.dev() != device || mounts.contains(path) {
        return Err(io::Error::other(format!(
            "{} is a mount point",
            path.display()
        )));
    }
    let mode = meta.permissions().mode();
    if mode & 0o700 != 0o700 {
        gone_is_fine(fs::set_permissions(
            path,
            Permissions::from_mode(mode | 0o700),
        ))?;
    }
    let entries = match fs::read_dir(path) {
        Ok(entries) => entries,
        Err(err) => return gone_is_fine(Err(err)),
    };
    let mut first_error = None;
    for entry in entries {
        let result = entry.and_then(|entry| match entry.metadata() {
            Ok(meta) => remove_tree(&entry.path(), &meta, device, mounts),
            Err(err) => gone_is_fine(Err(err)),
        });
        if let Err(err) = result {
            first_error.get_or_insert(err);
        }
    }
    match first_error {
        Some(err) => Err(err),
        None => gone_is_fine(fs::remove_dir(path)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clean::plan::{Category, RootCommand};
    use crate::clean::scan::ALLOWED_ROOTS;
    use crate::testdir::TestDir;
    use std::os::unix::process::ExitStatusExt;
    use std::process::ExitStatus;

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
        let mut log = Log::open(&setup.log_path, "clean").unwrap();
        let mut no_progress = |_: usize, _: usize, _: &str| {};
        let stop = AtomicBool::new(false);
        remove_home(
            &plan,
            &setup.rules,
            mounts,
            &mut log,
            &mut no_progress,
            &stop,
        )
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
    fn an_item_that_is_already_gone_counts_as_removed() {
        let s = setup(TestDir::new());
        s.home.dir(".cache");
        let gone = s.home.path().join(".cache/gone");
        let outcomes = apply_items(
            &s,
            vec![selected(Kind::Regenerable(gone))],
            &Mounts::default(),
        );
        assert!(outcomes[0].error.is_none(), "{:?}", outcomes[0]);
    }

    #[test]
    fn a_failed_log_write_does_not_fail_the_removal() {
        let s = setup(TestDir::new());
        let cache = s.home.dir(".cache/app");
        let mut category = Category::new("Test");
        category.items = vec![selected(Kind::Regenerable(cache.clone()))];
        let plan = Plan {
            categories: vec![category],
        };
        // Every write to /dev/full fails with "no space left".
        let mut log = Log::open(Path::new("/dev/full"), "clean").unwrap();
        let stop = AtomicBool::new(false);
        let outcomes = remove_home(
            &plan,
            &s.rules,
            &Mounts::default(),
            &mut log,
            &mut |_, _, _| {},
            &stop,
        );
        assert!(!cache.exists());
        assert!(outcomes[0].error.is_none());
        assert!(outcomes[0].log_error.is_some());
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
    fn keeps_the_trashinfo_of_what_could_not_be_removed() {
        let s = setup(TestDir::new());
        let stuck = s.home.file(".local/share/Trash/files/stuck/mounted/a", 10);
        s.home.file(".local/share/Trash/files/gone.txt", 10);
        let trash = s.home.path().join(".local/share/Trash");
        let stuck_info = s.home.file(".local/share/Trash/info/stuck.trashinfo", 10);
        let gone_info = s
            .home
            .file(".local/share/Trash/info/gone.txt.trashinfo", 10);
        let orphan_info = s.home.file(".local/share/Trash/info/orphan.trashinfo", 10);
        let mounts = Mounts::from_paths([stuck.parent().unwrap().to_path_buf()]);
        let outcomes = apply_items(&s, vec![selected(Kind::Trash(trash))], &mounts);
        assert!(outcomes[0].error.is_some());
        assert!(stuck.exists() && stuck_info.exists());
        assert!(!gone_info.exists() && !orphan_info.exists());
    }

    #[test]
    fn does_not_empty_a_mounted_trash_part() {
        let s = setup(TestDir::new());
        let mounted = s.home.file(".local/share/Trash/files/project", 10);
        s.home.file(".local/share/Trash/info/old.trashinfo", 10);
        let trash = s.home.path().join(".local/share/Trash");
        let mounts = Mounts::from_paths([trash.join("files")]);
        let outcomes = apply_items(&s, vec![selected(Kind::Trash(trash.clone()))], &mounts);
        assert!(outcomes[0].error.as_ref().unwrap().contains("mount point"));
        assert!(mounted.exists());
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
    fn stopping_leaves_the_remaining_items_alone() {
        let s = setup(TestDir::new());
        let first = s.home.dir(".cache/first");
        let second = s.home.dir(".cache/second");
        let mut category = Category::new("Test");
        category.items = vec![
            selected(Kind::Regenerable(first.clone())),
            selected(Kind::Regenerable(second.clone())),
        ];
        let plan = Plan {
            categories: vec![category],
        };
        let mut log = Log::open(&s.log_path, "clean").unwrap();
        let stop = AtomicBool::new(false);
        let mut seen = Vec::new();
        let mut progress = |done: usize, total: usize, _: &str| {
            seen.push((done, total));
            stop.store(true, Ordering::Relaxed);
        };
        let outcomes = remove_home(
            &plan,
            &s.rules,
            &Mounts::default(),
            &mut log,
            &mut progress,
            &stop,
        );
        assert_eq!(seen, [(0, 2)]);
        assert!(!first.exists() && second.exists());
        assert!(outcomes[0].error.is_none() && outcomes[1].error.is_some());
        assert_eq!(log_lines(&s).len(), 1);
    }

    #[test]
    fn keeps_root_results_when_the_tui_cannot_come_back() {
        let s = setup(TestDir::new());
        let mut category = Category::new("Root");
        category.items = vec![selected(Kind::Root(RootCommand::PacmanUninstalled))];
        let plan = Plan {
            categories: vec![category],
        };
        let mut log = Log::open(&s.log_path, "clean").unwrap();
        let mut run = |_: &mut [Command]| Ran {
            statuses: vec![Ok(ExitStatus::from_raw(0))],
            terminal: Err(io::Error::other("no tty")),
        };
        let (outcomes, terminal) = remove_root(&plan, &mut log, &mut run);
        assert!(outcomes[0].error.is_none());
        assert!(terminal.is_err());
        assert_eq!(log_lines(&s)[0]["removed"], true);
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
        let mut log = Log::open(&s.log_path, "clean").unwrap();
        let mut ran = Vec::new();
        let mut run = |commands: &mut [Command]| -> Ran {
            ran = commands.iter().map(crate::term::describe).collect();
            // The journal vacuum fails.
            Ran {
                statuses: vec![
                    Ok(ExitStatus::from_raw(0)),
                    Ok(ExitStatus::from_raw(0)),
                    Ok(ExitStatus::from_raw(1 << 8)),
                ],
                terminal: Ok(()),
            }
        };
        let (outcomes, terminal) = remove_root(&plan, &mut log, &mut run);
        assert!(terminal.is_ok());
        assert_eq!(
            ran,
            [
                "/usr/bin/sudo /usr/bin/paccache -r -k2",
                "/usr/bin/sudo /usr/bin/paccache -r -u -k0",
                "/usr/bin/sudo /usr/bin/journalctl --rotate --vacuum-size=200M",
            ]
        );
        let failed: Vec<bool> = outcomes.iter().map(|o| o.error.is_some()).collect();
        assert_eq!(failed, [false, false, true]);
        assert_eq!(log_lines(&s).len(), 3);
    }
}
