//! The Operation Log: one JSON line per Item vole tried to remove, and the outcome.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

/// What happened to one selected Item.
#[derive(Debug)]
pub struct Outcome {
    pub label: String,
    /// Bytes freed; for root commands, the Plan's estimate.
    pub size: u64,
    /// Why the Item wasn't (fully) removed.
    pub error: Option<String>,
    /// Why the attempt is missing from the Operation Log, whatever its result.
    pub log_error: Option<String>,
    /// Moved into a Backup rather than deleted: its space isn't freed yet.
    pub moved: bool,
}

/// The Operation Log, written to by one module.
pub struct Log {
    file: File,
    module: &'static str,
}

#[derive(Serialize)]
struct Entry<'a> {
    /// Seconds since the Unix epoch.
    time: u64,
    module: &'static str,
    item: &'a str,
    /// The removed path, or the command that was run.
    target: &'a str,
    /// Where `target` was moved to, for an Item moved into a Backup.
    #[serde(skip_serializing_if = "Option::is_none")]
    moved_to: Option<&'a str>,
    kind: &'static str,
    size: u64,
    /// Whether it was removed; for an attempt to remove it.
    #[serde(skip_serializing_if = "Option::is_none")]
    removed: Option<bool>,
    /// Whether it was put back where it was; for an attempt to restore it.
    #[serde(skip_serializing_if = "Option::is_none")]
    restored: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<&'a str>,
}

impl Log {
    fn path() -> Option<PathBuf> {
        dirs::state_dir().map(|dir| dir.join("vole").join("log.jsonl"))
    }

    /// Opens the log in its place, `~/.local/state/vole/log.jsonl`, for the attempts of
    /// `module`, or says why it can't.
    pub fn open_default(module: &'static str) -> Result<Self, String> {
        let path = Self::path().ok_or("the operation log's folder could not be found")?;
        Self::open(&path, module)
            .map_err(|err| format!("opening the operation log {}: {err}", path.display()))
    }

    /// Opens the log for the attempts of `module`.
    pub fn open(path: &Path, module: &'static str) -> io::Result<Self> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        Ok(Self { file, module })
    }

    /// Writes the attempt to remove `item` to the log and turns it into an Outcome. `target` is
    /// the removed path or the command that was run, and `size` what removing frees.
    pub fn record(
        &mut self,
        item: &str,
        kind: &'static str,
        size: u64,
        target: &str,
        result: Result<(), String>,
    ) -> Outcome {
        self.write_attempt(item, kind, size, target, None, false, result)
    }

    /// Writes the attempt to move `item` from `target` into a Backup at `to` to the log, and
    /// turns it into an Outcome.
    pub fn record_moved(
        &mut self,
        item: &str,
        kind: &'static str,
        size: u64,
        target: &str,
        to: &str,
        result: Result<(), String>,
    ) -> Outcome {
        self.write_attempt(item, kind, size, target, Some(to), false, result)
    }

    /// Writes the attempt to put `item` back from `target`, in a Backup, to `to`, where it was.
    pub fn record_restored(
        &mut self,
        item: &str,
        kind: &'static str,
        size: u64,
        target: &str,
        to: &str,
        result: Result<(), String>,
    ) -> Outcome {
        self.write_attempt(item, kind, size, target, Some(to), true, result)
    }

    #[allow(clippy::too_many_arguments)]
    fn write_attempt(
        &mut self,
        item: &str,
        kind: &'static str,
        size: u64,
        target: &str,
        moved_to: Option<&str>,
        restoring: bool,
        result: Result<(), String>,
    ) -> Outcome {
        let entry = Entry {
            time: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_secs()),
            module: self.module,
            item,
            target,
            moved_to: moved_to.filter(|_| result.is_ok()),
            kind,
            size,
            removed: (!restoring).then_some(result.is_ok()),
            restored: restoring.then_some(result.is_ok()),
            error: result.as_ref().err().map(String::as_str),
        };
        let written = self.write(&entry);
        Outcome {
            label: item.to_string(),
            size,
            error: result.err(),
            log_error: written
                .err()
                .map(|err| format!("not written to the operation log: {err}")),
            moved: moved_to.is_some() && !restoring,
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
    use crate::testdir::TestDir;

    #[test]
    fn writes_one_line_per_attempt() {
        let dir = TestDir::new();
        let path = dir.path().join("state/vole/log.jsonl");
        let mut log = Log::open(&path, "orphans").unwrap();
        let removed = log.record("a", "package", 10, "cmd a", Ok(()));
        let failed = log.record("b", "package", 20, "cmd b", Err("no".into()));
        assert!(removed.error.is_none() && removed.log_error.is_none());
        assert_eq!(failed.error.as_deref(), Some("no"));

        let lines: Vec<serde_json::Value> = fs::read_to_string(&path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["module"], "orphans");
        assert_eq!(lines[0]["removed"], true);
        assert!(lines[0].get("error").is_none());
        assert_eq!(lines[1]["item"], "b");
        assert_eq!(lines[1]["error"], "no");
        assert!(lines[0].get("moved_to").is_none());
    }

    #[test]
    fn says_where_a_moved_item_went() {
        let dir = TestDir::new();
        let path = dir.path().join("log.jsonl");
        let mut log = Log::open(&path, "orphans").unwrap();
        let moved = log.record_moved(
            "~/.config/a",
            "orphaned config",
            1,
            "/h/.config/a",
            "/b/.config/a",
            Ok(()),
        );
        assert!(moved.moved && moved.error.is_none());
        let failed = log.record_moved(
            "~/.config/b",
            "orphaned config",
            1,
            "/h/.config/b",
            "/b/.config/b",
            Err("no".into()),
        );
        assert!(failed.error.is_some());
        let lines: Vec<serde_json::Value> = fs::read_to_string(&path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(lines[0]["moved_to"], "/b/.config/a");
        assert_eq!(lines[0]["removed"], true);
        // Nothing was moved anywhere.
        assert!(lines[1].get("moved_to").is_none());
    }

    #[test]
    fn restores_are_not_removals() {
        let dir = TestDir::new();
        let path = dir.path().join("log.jsonl");
        let mut log = Log::open(&path, "restore").unwrap();
        let back = log.record_restored(
            "~/.config/a",
            "orphaned config",
            1,
            "/b/.config/a",
            "/h/.config/a",
            Ok(()),
        );
        assert!(back.error.is_none() && !back.moved);
        let line: serde_json::Value =
            serde_json::from_str(fs::read_to_string(&path).unwrap().trim()).unwrap();
        assert_eq!(line["restored"], true);
        assert!(line.get("removed").is_none());
        assert_eq!(line["moved_to"], "/h/.config/a");
    }
}
