//! Backups: dated snapshots of the Orphaned Configs vole removed, in
//! `~/.local/share/vole/backups/<date>/`, from which they can be put back.
//!
//! A Backup keeps each config where it was relative to the home directory (`~/.config/foo` is
//! kept as `<backup>/.config/foo`), and lists them in `vole-backup.jsonl`, one JSON line each.

use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{self, ErrorKind, Write};
use std::os::unix::fs::DirBuilderExt;
use std::path::{Component, Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use super::configs;
use crate::date;
use crate::disk::{self, Mounts};
use crate::safety::Rules;

/// Where the Backups are, relative to the home directory.
pub const BACKUPS: &str = ".local/share/vole/backups";

/// The list of what a Backup holds, in the Backup's folder. Configs moved in all start with a
/// dot or are inside a dotfolder, so they can't take its name.
const MANIFEST: &str = "vole-backup.jsonl";

/// One config in a Backup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// Where it was, relative to the home directory.
    pub path: PathBuf,
    /// Its size when it was moved.
    pub size: u64,
}

/// Backups this many days old or more are proposed for deleting.
pub const OLD_DAYS: u64 = 30;

/// A Backup as found on disk.
#[derive(Debug, Clone)]
pub struct Backup {
    pub dir: PathBuf,
    /// When it was made, from its name.
    pub created: SystemTime,
    /// What it holds, as its list says; lines that can't be read are left out.
    pub entries: Vec<Entry>,
}

/// The Backups in the home directory, oldest first: the folders in the backups folder (not
/// symlinks) named like a Backup. Anything else there is left alone.
pub fn list(home: &Path) -> Vec<Backup> {
    let mut found = Vec::new();
    for entry in fs::read_dir(home.join(BACKUPS))
        .into_iter()
        .flatten()
        .flatten()
    {
        if !entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let Some(created) = created(&entry.file_name().to_string_lossy()) else {
            continue;
        };
        let dir = entry.path();
        let entries = read_entries(&dir);
        found.push(Backup {
            dir,
            created,
            entries,
        });
    }
    found.sort_by(|a, b| (a.created, &a.dir).cmp(&(b.created, &b.dir)));
    found
}

/// What the Backup at `dir` lists; lines that can't be read are left out.
fn read_entries(dir: &Path) -> Vec<Entry> {
    fs::read_to_string(dir.join(MANIFEST))
        .unwrap_or_default()
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect()
}

/// Puts the config `entry` lists in the Backup at `dir` back where it was in `home`, and
/// returns where. It never overwrites: when something is there now, the config stays in the
/// Backup. Only places configs are looked for are written to, by the path rules.
pub fn put_back(home: &Path, rules: &Rules, dir: &Path, entry: &Entry) -> Result<PathBuf, String> {
    let relative = &entry.path;
    let shown = format!("~/{}", relative.display());
    let plain = relative
        .components()
        .all(|component| matches!(component, Component::Normal(_)));
    if relative.as_os_str().is_empty() || !plain {
        return Err(format!("refused: {shown} is not a path in the home folder"));
    }
    let target = home.join(relative);
    if !configs::is_candidate_path(home, &target) {
        return Err(format!("refused: {shown} is not where configs are kept"));
    }
    let source = dir.join(relative);
    match fs::symlink_metadata(&source) {
        Ok(_) => {}
        Err(err) if err.kind() == ErrorKind::NotFound => {
            return Err(format!("{shown} isn't in the Backup anymore"));
        }
        Err(err) => return Err(format!("{}: {err}", source.display())),
    }
    match fs::symlink_metadata(&target) {
        Ok(_) => return Err(format!("{shown} exists now; move it away to put this back")),
        Err(err) if err.kind() == ErrorKind::NotFound => {}
        Err(err) => return Err(format!("{shown}: {err}")),
    }
    // The folder it was in may be gone, like ~/.local/state.
    if let Some(parent) = target.parent() {
        make_real_dirs(home, parent).map_err(|err| format!("{}: {err}", parent.display()))?;
    }
    let real = rules
        .check_put_back(&target)
        .map_err(|refusal| format!("refused: {shown} is {refusal}"))?;
    match disk::rename_no_overwrite(&source, &real) {
        Ok(()) => Ok(real),
        // Made between the check above and now.
        Err(err) if err.kind() == ErrorKind::AlreadyExists => {
            Err(format!("{shown} exists now; move it away to put this back"))
        }
        Err(err) => Err(format!("{shown}: {err}")),
    }
}

/// Makes the folders from `home` down to `dir` that are missing, going only through real
/// folders: never through a symlink, so nothing is made outside the home directory.
fn make_real_dirs(home: &Path, dir: &Path) -> io::Result<()> {
    let relative = dir
        .strip_prefix(home)
        .map_err(|_| io::Error::other("outside the home folder"))?;
    let mut current = home.to_path_buf();
    for component in relative.components() {
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(meta) if meta.is_dir() => {}
            Ok(_) => {
                return Err(io::Error::other(format!(
                    "{} is not a folder",
                    current.display()
                )));
            }
            Err(err) if err.kind() == ErrorKind::NotFound => fs::create_dir(&current)?,
            Err(err) => return Err(err),
        }
    }
    Ok(())
}

/// Takes `restored` off the list of the Backup at `dir`. A Backup left listing nothing goes,
/// with the folders that held what was put back, as long as they are empty.
pub fn forget(dir: &Path, restored: &[Entry]) -> io::Result<()> {
    let remaining: Vec<Entry> = read_entries(dir)
        .into_iter()
        .filter(|entry| !restored.contains(entry))
        .collect();
    if remaining.is_empty() {
        fs::remove_file(dir.join(MANIFEST))?;
        return remove_empty_dirs(dir);
    }
    let mut text = String::new();
    for entry in &remaining {
        text.push_str(&serde_json::to_string(entry)?);
        text.push('\n');
    }
    let rewritten = dir.join(format!("{MANIFEST}.new"));
    fs::write(&rewritten, text)?;
    fs::rename(rewritten, dir.join(MANIFEST))
}

/// Removes `dir` and the folders in it, deepest first, as long as each is empty; anything else
/// stays, and so do the folders holding it.
fn remove_empty_dirs(dir: &Path) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            remove_empty_dirs(&entry.path())?;
        }
    }
    match fs::remove_dir(dir) {
        Err(err) if err.kind() == ErrorKind::DirectoryNotEmpty => Ok(()),
        result => result,
    }
}

/// When the Backup called `name` was made: its stamp, maybe with `-2` and so on after it.
fn created(name: &str) -> Option<SystemTime> {
    date::parse_stamp(name).or_else(|| {
        let (stamp, number) = name.rsplit_once('-')?;
        if number.is_empty() || !number.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        date::parse_stamp(stamp)
    })
}

/// A Backup old enough to be proposed for deleting: an Item of the Plan.
#[derive(Debug, Clone)]
pub struct OldBackup {
    pub path: PathBuf,
    /// E.g. `Backup from 5 Oct 2026, 13:30 UTC`.
    pub label: String,
    /// How many configs it holds.
    pub configs: usize,
    pub size: u64,
    pub selected: bool,
}

/// The Backups [`OLD_DAYS`] old or more, measured, oldest first.
pub fn old(home: &Path, mounts: &Mounts, now: SystemTime) -> Vec<OldBackup> {
    list(home)
        .into_iter()
        .filter(|backup| date::days_between(backup.created, now) >= OLD_DAYS)
        .map(|backup| OldBackup {
            label: format!("Backup from {}", date::day_and_time(backup.created)),
            configs: backup.entries.len(),
            size: disk::usage(&backup.dir, mounts).bytes,
            path: backup.dir,
            selected: false,
        })
        .collect()
}

/// A Backup being filled.
pub struct NewBackup {
    dir: PathBuf,
    manifest: File,
}

impl NewBackup {
    /// Creates a Backup named after `now` in the home directory's backups folder; a second one
    /// in the same second gets `-2` after the name, and so on.
    pub fn create(home: &Path, now: SystemTime) -> io::Result<Self> {
        // Configs can hold private things: only the user may look inside.
        let mut private = DirBuilder::new();
        private.mode(0o700);
        let backups = home.join(BACKUPS);
        DirBuilder::new()
            .mode(0o700)
            .recursive(true)
            .create(&backups)?;
        let stamp = date::stamp(now);
        let mut dir = backups.join(&stamp);
        let mut number = 2;
        loop {
            match private.create(&dir) {
                Ok(()) => break,
                Err(err) if err.kind() == ErrorKind::AlreadyExists => {
                    dir = backups.join(format!("{stamp}-{number}"));
                    number += 1;
                }
                Err(err) => return Err(err),
            }
        }
        let manifest = OpenOptions::new()
            .create_new(true)
            .append(true)
            .open(dir.join(MANIFEST))?;
        Ok(Self { dir, manifest })
    }

    #[cfg(test)]
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Moves `path`, which is `relative` to the home directory, into the Backup at the same
    /// place, and returns where it went. It is listed first, so nothing is ever in a Backup
    /// without being listed, and unlisted again if the move fails. The move is a rename: it
    /// fails rather than copy across filesystems.
    pub fn take(&mut self, path: &Path, relative: &Path, size: u64) -> io::Result<PathBuf> {
        let to = self.dir.join(relative);
        if let Some(parent) = to.parent() {
            fs::create_dir_all(parent)?;
        }
        let entry = Entry {
            path: relative.to_path_buf(),
            size,
        };
        let mut line = serde_json::to_string(&entry)?;
        line.push('\n');
        let listed = self.manifest.metadata()?.len();
        self.manifest.write_all(line.as_bytes())?;
        if let Err(err) = fs::rename(path, &to) {
            // Appending goes on at the end, wherever that now is.
            let _ = self.manifest.set_len(listed);
            return Err(err);
        }
        Ok(to)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testdir::TestDir;
    use std::time::{Duration, UNIX_EPOCH};

    fn entries(dir: &Path) -> Vec<Entry> {
        fs::read_to_string(dir.join(MANIFEST))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    #[test]
    fn keeps_configs_where_they_were_and_lists_them() {
        let home = TestDir::new();
        home.file(".config/gone/settings", 10);
        let dotfile = home.file(".gonerc", 10);
        let now = UNIX_EPOCH + Duration::from_secs(1_791_207_000);
        let mut backup = NewBackup::create(home.path(), now).unwrap();
        assert_eq!(
            backup.dir(),
            home.path()
                .join(".local/share/vole/backups/2026-10-05T13-30-00Z")
        );
        let moved = backup
            .take(
                &home.path().join(".config/gone"),
                Path::new(".config/gone"),
                4096,
            )
            .unwrap();
        assert_eq!(moved, backup.dir().join(".config/gone"));
        assert!(moved.join("settings").exists());
        assert!(!home.path().join(".config/gone").exists());
        backup.take(&dotfile, Path::new(".gonerc"), 10).unwrap();
        assert_eq!(
            entries(backup.dir()),
            [
                Entry {
                    path: ".config/gone".into(),
                    size: 4096
                },
                Entry {
                    path: ".gonerc".into(),
                    size: 10
                },
            ]
        );
    }

    #[test]
    fn a_second_backup_in_the_same_second_gets_a_number() {
        let home = TestDir::new();
        let now = UNIX_EPOCH + Duration::from_secs(1_791_207_000);
        let first = NewBackup::create(home.path(), now).unwrap();
        let second = NewBackup::create(home.path(), now).unwrap();
        assert_ne!(first.dir(), second.dir());
        assert!(second.dir().ends_with("2026-10-05T13-30-00Z-2"));
    }

    #[test]
    fn lists_backups_oldest_first_and_finds_the_old_ones() {
        let home = TestDir::new();
        let day = Duration::from_secs(86_400);
        let now = UNIX_EPOCH + Duration::from_secs(1_791_207_000);
        let mut old = NewBackup::create(home.path(), now - 40 * day).unwrap();
        home.file(".config/gone/settings", 10);
        old.take(
            &home.path().join(".config/gone"),
            Path::new(".config/gone"),
            4096,
        )
        .unwrap();
        let again = NewBackup::create(home.path(), now - 40 * day).unwrap();
        let recent = NewBackup::create(home.path(), now - 2 * day).unwrap();
        // Not Backups: left alone.
        home.dir(".local/share/vole/backups/notes");
        home.file(".local/share/vole/backups/2026-10-05T13-30-00Z.txt", 1);
        home.symlink(
            ".local/share/vole/backups/2026-10-05T13-30-01Z",
            &home.dir("elsewhere"),
        );

        let backups = list(home.path());
        let dirs: Vec<&Path> = backups.iter().map(|b| b.dir.as_path()).collect();
        assert_eq!(dirs, [old.dir(), again.dir(), recent.dir()]);
        assert_eq!(backups[0].created, now - 40 * day);
        assert_eq!(backups[1].created, now - 40 * day);
        assert_eq!(backups[0].entries.len(), 1);

        let found = old_backups(home.path(), now);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].label, "Backup from 26 Aug 2026, 13:30 UTC");
        assert_eq!(found[0].configs, 1);
        assert!(found[0].size > found[1].size);
        assert!(!found[0].selected);
    }

    fn old_backups(home: &Path, now: SystemTime) -> Vec<OldBackup> {
        old(home, &Mounts::default(), now)
    }

    fn rules(home: &TestDir) -> Rules {
        Rules::new(home.path(), configs::ROOTS, &[])
            .unwrap()
            .with_loose_dotfiles()
    }

    #[test]
    fn puts_configs_back_and_forgets_them() {
        let home = TestDir::new();
        home.file(".config/gone/settings", 10);
        let rc = home.file(".gonerc", 10);
        let now = UNIX_EPOCH + Duration::from_secs(1_791_207_000);
        let mut backup = NewBackup::create(home.path(), now).unwrap();
        let dir = backup.dir().to_path_buf();
        backup
            .take(
                &home.path().join(".config/gone"),
                Path::new(".config/gone"),
                4096,
            )
            .unwrap();
        backup.take(&rc, Path::new(".gonerc"), 10).unwrap();
        let entries = list(home.path())[0].entries.clone();

        let rules = rules(&home);
        let back = put_back(home.path(), &rules, &dir, &entries[0]).unwrap();
        assert_eq!(back, home.path().join(".config/gone"));
        assert!(home.path().join(".config/gone/settings").exists());
        forget(&dir, &entries[..1]).unwrap();
        assert_eq!(list(home.path())[0].entries, entries[1..]);

        put_back(home.path(), &rules, &dir, &entries[1]).unwrap();
        assert!(rc.exists());
        forget(&dir, &entries[1..]).unwrap();
        // Nothing left: the Backup goes.
        assert!(!dir.exists());
        assert!(list(home.path()).is_empty());
    }

    #[test]
    fn never_overwrites_and_refuses_odd_paths() {
        let home = TestDir::new();
        home.file(".config/app/settings", 10);
        let now = UNIX_EPOCH + Duration::from_secs(1_791_207_000);
        let mut backup = NewBackup::create(home.path(), now).unwrap();
        let dir = backup.dir().to_path_buf();
        backup
            .take(
                &home.path().join(".config/app"),
                Path::new(".config/app"),
                1,
            )
            .unwrap();
        // The program made a new one meanwhile.
        home.file(".config/app/new", 10);
        let rules = rules(&home);
        let entry = |path: &str| Entry {
            path: path.into(),
            size: 1,
        };
        let err = put_back(home.path(), &rules, &dir, &entry(".config/app")).unwrap_err();
        assert!(err.contains("exists now"), "{err}");
        assert!(dir.join(".config/app/settings").exists());
        for odd in [
            "../outside",
            "/etc/passwd",
            ".config/app/deep",
            "Documents",
            ".ssh",
            "",
        ] {
            let err = put_back(home.path(), &rules, &dir, &entry(odd)).unwrap_err();
            assert!(
                err.contains("refused") || err.contains("isn't in"),
                "{odd}: {err}"
            );
        }
    }

    #[test]
    fn puts_back_without_making_folders_through_symlinks() {
        let home = TestDir::new();
        let elsewhere = TestDir::new();
        home.file(".local/state/app/data", 1);
        let now = UNIX_EPOCH + Duration::from_secs(1_791_207_000);
        let mut backup = NewBackup::create(home.path(), now).unwrap();
        let dir = backup.dir().to_path_buf();
        backup
            .take(
                &home.path().join(".local/state/app"),
                Path::new(".local/state/app"),
                1,
            )
            .unwrap();
        // ~/.local/state is gone: it is made again.
        fs::remove_dir(home.path().join(".local/state")).unwrap();
        let rules = rules(&home);
        let entry = |path: &str| Entry {
            path: path.into(),
            size: 1,
        };
        put_back(home.path(), &rules, &dir, &entry(".local/state/app")).unwrap();
        assert!(home.path().join(".local/state/app/data").exists());

        // ~/.local became a symlink: nothing is made through it.
        let mut backup = NewBackup::create(home.path(), now).unwrap();
        let dir = backup.dir().to_path_buf();
        home.file(".cache/app/data", 1);
        backup
            .take(&home.path().join(".cache/app"), Path::new(".cache/app"), 1)
            .unwrap();
        fs::remove_dir_all(home.path().join(".cache")).unwrap();
        std::os::unix::fs::symlink(elsewhere.path(), home.path().join(".cache")).unwrap();
        assert!(put_back(home.path(), &rules, &dir, &entry(".cache/app")).is_err());
        assert!(fs::read_dir(elsewhere.path()).unwrap().next().is_none());
    }

    #[test]
    fn a_failed_move_leaves_the_config_in_place() {
        let home = TestDir::new();
        let now = UNIX_EPOCH + Duration::from_secs(1_791_207_000);
        let mut backup = NewBackup::create(home.path(), now).unwrap();
        let missing = home.path().join(".config/missing");
        assert!(
            backup
                .take(&missing, Path::new(".config/missing"), 1)
                .is_err()
        );
        assert!(!backup.dir().join(".config/missing").exists());
        // Not listed either, and what moves next is.
        assert!(entries(backup.dir()).is_empty());
        let rc = home.file(".gonerc", 1);
        backup.take(&rc, Path::new(".gonerc"), 1).unwrap();
        assert_eq!(entries(backup.dir()).len(), 1);
    }

    #[test]
    fn backups_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let home = TestDir::new();
        let now = UNIX_EPOCH + Duration::from_secs(1_791_207_000);
        let backup = NewBackup::create(home.path(), now).unwrap();
        for dir in [backup.dir(), &home.path().join(BACKUPS)] {
            let mode = fs::metadata(dir).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o700, "{}", dir.display());
        }
    }
}
