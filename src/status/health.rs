//! The health panel: what vole can clean up, and the updates pacman would install, measured by
//! the official tools, each on a thread of its own, so a slow one doesn't hold the others up.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use anyhow::{Context, Result, bail};

use crate::disk::{self, Mounts};
use crate::{clean, orphans, pacman, size};

/// A row of the health panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Check {
    PacmanCache,
    Journal,
    OrphanedPackages,
    PendingUpdates,
}

impl Check {
    pub const ALL: [Check; 4] = [
        Check::PacmanCache,
        Check::Journal,
        Check::OrphanedPackages,
        Check::PendingUpdates,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Check::PacmanCache => "Pacman cache",
            Check::Journal => "Journal",
            Check::OrphanedPackages => "Orphaned packages",
            Check::PendingUpdates => "Pending updates",
        }
    }

    /// What the row says, measured now.
    fn measure(self) -> Result<String> {
        match self {
            Check::PacmanCache => pacman_cache().map(size::format),
            Check::Journal => {
                let usage = size::format(clean::journal_usage()?);
                // Only the files the user can read are counted.
                Ok(if clean::sees_whole_journal() {
                    usage
                } else {
                    format!("≥ {usage}")
                })
            }
            Check::OrphanedPackages => orphans::package_count().map(count),
            Check::PendingUpdates => pending_updates().map(count),
        }
    }
}

/// `n` as a row says it: `none` for 0.
fn count(n: usize) -> String {
    match n {
        0 => "none".into(),
        n => n.to_string(),
    }
}

/// What a row says once measured, or why it couldn't be.
pub type Measured = Result<String, String>;

/// The rows of the health panel, measured in the background.
pub struct Health {
    /// What each row of [`Check::ALL`] said last.
    rows: [Option<Measured>; Check::ALL.len()],
    /// Which rows are being measured.
    measuring: [bool; Check::ALL.len()],
    sender: Sender<(Check, Measured)>,
    receiver: Receiver<(Check, Measured)>,
}

impl Default for Health {
    fn default() -> Self {
        let (sender, receiver) = mpsc::channel();
        Self {
            rows: Default::default(),
            measuring: Default::default(),
            sender,
            receiver,
        }
    }
}

impl Health {
    /// Measures each row again, unless it is being measured still. Until it is, a row keeps
    /// what it said.
    pub fn measure(&mut self) {
        for check in Check::ALL {
            if std::mem::replace(&mut self.measuring[check as usize], true) {
                continue;
            }
            let sender = self.sender.clone();
            thread::spawn(move || {
                // Shown on one row: the first line of the error.
                let measured = check
                    .measure()
                    .map_err(|err| format!("{err:#}").lines().next().unwrap_or("").into());
                let _ = sender.send((check, measured));
            });
        }
    }

    /// Takes in what was measured since.
    pub fn update(&mut self) {
        while let Ok((check, measured)) = self.receiver.try_recv() {
            self.set(check, measured);
        }
    }

    /// What `check`'s row says; `None` until it is first measured.
    pub fn row(&self, check: Check) -> Option<&Measured> {
        self.rows[check as usize].as_ref()
    }

    fn set(&mut self, check: Check, measured: Measured) {
        self.rows[check as usize] = Some(measured);
        self.measuring[check as usize] = false;
    }

    #[cfg(test)]
    pub fn with(rows: &[(Check, Measured)]) -> Self {
        let mut health = Self::default();
        for (check, measured) in rows {
            health.set(*check, measured.clone());
        }
        health
    }
}

const CHECKUPDATES: &str = "/usr/bin/checkupdates";

/// How many packages the repositories have newer versions of, as `checkupdates` says. It syncs a
/// copy of the package databases in a folder of its own, never pacman's, so a later `pacman -S`
/// still can't make a partial upgrade.
fn pending_updates() -> Result<usize> {
    if !Path::new(CHECKUPDATES).exists() {
        bail!("checkupdates not found; install pacman-contrib");
    }
    let output = Command::new(CHECKUPDATES)
        .arg("--nocolor")
        .env("LC_ALL", "C")
        .output()
        .with_context(|| format!("running {CHECKUPDATES}"))?;
    updates(&output)
}

/// Reads what `checkupdates` said: a line per package, e.g. `linux 6.17.1-1 -> 6.17.2-1`, or
/// exit status 2 for none.
fn updates(output: &Output) -> Result<usize> {
    match output.status.code() {
        Some(0) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            Ok(stdout
                .lines()
                .filter(|line| !line.trim().is_empty())
                .count())
        }
        Some(2) => Ok(0),
        _ => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let error = stderr
                .lines()
                .map(|line| line.trim_start_matches("==> ERROR:").trim())
                .find(|line| !line.is_empty());
            match error {
                Some(error) => bail!("{error}"),
                None => bail!("checkupdates failed ({})", output.status),
            }
        }
    }
}

/// The space pacman's package cache takes on disk.
fn pacman_cache() -> Result<u64> {
    let dirs = pacman::cache_dirs().context("asking pacman-conf where the cache is")?;
    Ok(space_taken(&dirs))
}

/// The space the folders `dirs` take on disk, where they really are, each once.
fn space_taken(dirs: &[PathBuf]) -> u64 {
    // pacman follows a symlink to a folder; the walk wouldn't.
    let mut dirs: Vec<PathBuf> = dirs
        .iter()
        .filter_map(|dir| fs::canonicalize(dir).ok())
        .collect();
    // A folder listed twice, or inside another, would count twice.
    dirs.sort();
    dirs.dedup_by(|inside, folder| inside.starts_with(folder));
    // Without the mount points, as a folder may be one, like a btrfs subvolume for the cache:
    // the walk still stays on the filesystem each folder is on. One walk, so a package linked
    // from two of the folders counts once.
    disk::disk_usage_all(&dirs, &Mounts::default())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn output(code: i32, stdout: &str, stderr: &str) -> Output {
        use std::os::unix::process::ExitStatusExt;
        Output {
            status: std::process::ExitStatus::from_raw(code << 8),
            stdout: stdout.into(),
            stderr: stderr.into(),
        }
    }

    #[test]
    fn reads_what_checkupdates_says() {
        let pending = "linux 6.17.1.arch1-1 -> 6.17.2.arch1-1\nmesa 1:25.2.4-1 -> 1:25.2.5-1\n";
        assert_eq!(updates(&output(0, pending, "")).unwrap(), 2);
        assert_eq!(updates(&output(2, "", "")).unwrap(), 0);
        let offline = output(1, "", "==> ERROR: Cannot fetch updates\n");
        assert_eq!(
            updates(&offline).unwrap_err().to_string(),
            "Cannot fetch updates"
        );
        assert_eq!(
            updates(&output(1, "", "")).unwrap_err().to_string(),
            "checkupdates failed (exit status: 1)"
        );
    }

    #[test]
    fn measures_each_cache_folder_once_where_it_really_is() {
        let dir = crate::testdir::TestDir::new();
        let cache = dir.dir("cache");
        dir.file("cache/a.pkg.tar.zst", 50_000);
        dir.file("cache/aur/b.pkg.tar.zst", 50_000);
        let link = dir.symlink("link", &cache);
        let once = space_taken(std::slice::from_ref(&cache));
        assert!(once > 100_000, "{once}");
        let listed = [
            link,
            cache.clone(),
            cache.join("aur"),
            cache.join("../cache"),
        ];
        assert_eq!(space_taken(&listed), once);
    }

    #[test]
    fn counts_say_none_for_nothing() {
        assert_eq!(count(0), "none");
        assert_eq!(count(27), "27");
    }

    #[test]
    fn rows_keep_what_they_said_until_measured_again() {
        let mut health = Health::with(&[(Check::Journal, Ok("2.1 GiB".into()))]);
        assert_eq!(health.row(Check::PacmanCache), None);
        health.measuring = [true; Check::ALL.len()];
        // Being measured still: not again.
        health.measure();
        assert_eq!(health.row(Check::Journal), Some(&Ok("2.1 GiB".into())));
        health
            .sender
            .send((Check::Journal, Err("journalctl failed".into())))
            .unwrap();
        health.update();
        assert_eq!(
            health.row(Check::Journal),
            Some(&Err("journalctl failed".into()))
        );
        assert!(!health.measuring[Check::Journal as usize]);
    }
}
