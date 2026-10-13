//! The health panel: what vole can clean up, and the updates pacman would install, measured by
//! the official tools, each on a thread of its own, so a slow one doesn't hold the others up.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

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

    /// How long what the row says may be shown without measuring it again: pending updates,
    /// which checkupdates goes online for and clean and orphans don't change, half an hour; the
    /// others not at all.
    fn keeps(self) -> Duration {
        match self {
            Check::PendingUpdates => Duration::from_secs(30 * 60),
            Check::PacmanCache | Check::Journal | Check::OrphanedPackages => Duration::ZERO,
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
    /// The rows of [`Check::ALL`], in order.
    rows: [Row; Check::ALL.len()],
    sender: Sender<(Check, Measured)>,
    receiver: Receiver<(Check, Measured)>,
    /// Measures a check in the background, and sends what it says.
    run: fn(Check, Sender<(Check, Measured)>),
}

/// A row of the health panel.
#[derive(Debug, Default)]
struct Row {
    /// What it said last.
    shown: Option<Measured>,
    /// When it was last asked to be measured.
    started: Option<Instant>,
    measuring: bool,
    /// Whether it is to be measured again once it is done: it was asked to be while it was
    /// being measured, so what it says may be from before.
    again: bool,
}

impl Default for Health {
    fn default() -> Self {
        let (sender, receiver) = mpsc::channel();
        Self {
            rows: Default::default(),
            sender,
            receiver,
            run: |check, sender| {
                thread::spawn(move || {
                    // Shown on one row: the first line of the error.
                    let measured = check
                        .measure()
                        .map_err(|err| format!("{err:#}").lines().next().unwrap_or("").into());
                    let _ = sender.send((check, measured));
                });
            },
        }
    }
}

impl Health {
    /// Measures each row again, unless it was just measured and doesn't change that quickly.
    /// A row being measured still is measured again once it is done. Until then, a row keeps
    /// what it said.
    pub fn measure(&mut self) {
        self.measure_at(Instant::now());
    }

    /// [`Health::measure`], `now`.
    fn measure_at(&mut self, now: Instant) {
        for check in Check::ALL {
            let row = &mut self.rows[check as usize];
            if row
                .started
                .is_some_and(|at| now.duration_since(at) < check.keeps())
            {
                continue;
            }
            if row.measuring {
                row.again = true;
            } else {
                self.start(check, now);
            }
        }
    }

    /// Takes in what was measured since, and measures again what was asked to be meanwhile.
    pub fn update(&mut self) {
        while let Ok((check, measured)) = self.receiver.try_recv() {
            let row = &mut self.rows[check as usize];
            row.measuring = false;
            if std::mem::take(&mut row.again) {
                self.start(check, Instant::now());
            } else {
                row.shown = Some(measured);
            }
        }
    }

    /// What `check`'s row says; `None` until it is first measured.
    pub fn row(&self, check: Check) -> Option<&Measured> {
        self.rows[check as usize].shown.as_ref()
    }

    fn start(&mut self, check: Check, now: Instant) {
        let row = &mut self.rows[check as usize];
        row.measuring = true;
        row.started = Some(now);
        (self.run)(check, self.sender.clone());
    }

    #[cfg(test)]
    pub fn with(rows: &[(Check, Measured)]) -> Self {
        let mut health = Self::default();
        for (check, measured) in rows {
            health.rows[*check as usize].shown = Some(measured.clone());
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

    /// Health that measures nothing: what was measured is sent by hand.
    fn health() -> Health {
        Health {
            run: |_, _| {},
            ..Health::default()
        }
    }

    fn measuring(health: &Health) -> Vec<Check> {
        let measuring = Check::ALL.into_iter();
        measuring
            .filter(|&check| health.rows[check as usize].measuring)
            .collect()
    }

    #[test]
    fn rows_keep_what_they_said_until_measured_again() {
        let mut health = health();
        health.measure();
        assert_eq!(measuring(&health), Check::ALL);
        assert_eq!(health.row(Check::PacmanCache), None);
        let journal = |text: &str| (Check::Journal, Ok(text.to_string()));
        health.sender.send(journal("2.1 GiB")).unwrap();
        health.update();
        assert_eq!(health.row(Check::Journal), Some(&Ok("2.1 GiB".into())));
        // Measured again, it says what it said until it is done.
        health.measure();
        assert_eq!(health.row(Check::Journal), Some(&Ok("2.1 GiB".into())));
        health.sender.send(journal("50.0 MiB")).unwrap();
        health.update();
        assert_eq!(health.row(Check::Journal), Some(&Ok("50.0 MiB".into())));
    }

    #[test]
    fn a_row_asked_again_while_measured_is_measured_once_more() {
        let mut health = health();
        health.measure();
        // Asked again, as when the dashboard comes back from orphans, before the count is in.
        health.measure();
        let count = |n: &str| (Check::OrphanedPackages, Ok(n.to_string()));
        health.sender.send(count("12")).unwrap();
        health.update();
        // That count may be from before the orphans went: it is measured again.
        assert_eq!(health.row(Check::OrphanedPackages), None);
        assert!(measuring(&health).contains(&Check::OrphanedPackages));
        health.sender.send(count("none")).unwrap();
        health.update();
        assert_eq!(
            health.row(Check::OrphanedPackages),
            Some(&Ok("none".into()))
        );
    }

    #[test]
    fn pending_updates_are_checked_at_most_every_half_hour() {
        let mut health = health();
        health.measure();
        let updates = (Check::PendingUpdates, Ok("12".to_string()));
        health.sender.send(updates).unwrap();
        health.update();
        health.measure();
        assert!(!measuring(&health).contains(&Check::PendingUpdates));
        health.measure_at(Instant::now() + Check::PendingUpdates.keeps());
        assert!(measuring(&health).contains(&Check::PendingUpdates));
    }
}
