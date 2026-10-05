//! Backups: dated snapshots of the Orphaned Configs vole removed, in
//! `~/.local/share/vole/backups/<date>/`, from which they can be put back.
//!
//! A Backup keeps each config where it was relative to the home directory (`~/.config/foo` is
//! kept as `<backup>/.config/foo`), and lists them in `vole-backup.jsonl`, one JSON line each.

use std::fs::{self, File, OpenOptions};
use std::io::{self, ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::date;

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

/// A Backup being filled.
pub struct NewBackup {
    dir: PathBuf,
    manifest: File,
}

impl NewBackup {
    /// Creates a Backup named after `now` in the home directory's backups folder; a second one
    /// in the same second gets `-2` after the name, and so on.
    pub fn create(home: &Path, now: SystemTime) -> io::Result<Self> {
        let backups = home.join(BACKUPS);
        fs::create_dir_all(&backups)?;
        let stamp = date::stamp(now);
        let mut dir = backups.join(&stamp);
        let mut number = 2;
        loop {
            match fs::create_dir(&dir) {
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
    /// without being listed; if the move then fails, the list has a line for nothing, which is
    /// harmless. The move is a rename: it fails rather than copy across filesystems.
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
        self.manifest.write_all(line.as_bytes())?;
        fs::rename(path, &to)?;
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
    }
}
