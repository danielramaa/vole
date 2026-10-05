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
        let entry = Entry {
            time: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_secs()),
            module: self.module,
            item,
            target,
            kind,
            size,
            removed: result.is_ok(),
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
    }
}
