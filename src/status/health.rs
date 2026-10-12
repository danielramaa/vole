//! The health panel: what vole can clean up, measured by the official tools, each on a thread of
//! its own, so a slow one doesn't hold the others up.

use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use anyhow::{Context, Result};

use crate::disk::{self, Mounts};
use crate::{clean, pacman, size};

/// A row of the health panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Check {
    PacmanCache,
    Journal,
}

impl Check {
    pub const ALL: [Check; 2] = [Check::PacmanCache, Check::Journal];

    pub fn label(self) -> &'static str {
        match self {
            Check::PacmanCache => "Pacman cache",
            Check::Journal => "Journal",
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
        }
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

/// The space pacman's package cache takes on disk.
fn pacman_cache() -> Result<u64> {
    let dirs = pacman::cache_dirs().context("asking pacman-conf where the cache is")?;
    // Measuring only: without the mount points, the walk still stops at other filesystems.
    let mounts = Mounts::load().unwrap_or_default();
    Ok(dirs.iter().map(|dir| disk::disk_usage(dir, &mounts)).sum())
}

#[cfg(test)]
mod tests {
    use super::*;

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
