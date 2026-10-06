//! Moving the Items picked by hand to the desktop trash: only from inside the home folder, and
//! never a Sealed Path.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::config::Config;
use crate::disk::Mounts;
use crate::in_use;
use crate::log::{Log, Outcome};
use crate::safety::Rules;

/// GLib's gio, which finds the trash of each filesystem, and never deletes what it can't move
/// there.
pub const GIO: &str = "/usr/bin/gio";

/// What the Operation Log calls an Item picked by hand.
const KIND: &str = "hand-picked";

/// The desktop trash, relative to the home folder.
const TRASH: &str = ".local/share/Trash";

/// An Item picked by hand.
#[derive(Debug, Clone)]
pub struct Picked {
    pub path: PathBuf,
    pub label: String,
    pub size: u64,
    /// Held by a running process.
    pub in_use: bool,
}

/// What may be picked by hand.
pub struct Picker {
    rules: Rules,
    mounts: Mounts,
    trash: PathBuf,
    /// vole's own folders: its config, its Backups and its Operation Log.
    own: Vec<PathBuf>,
}

impl Picker {
    /// What may be picked in `home` (where it really is), with the mount points as they are now.
    pub fn load(home: &Path, config: &Config) -> Result<Self, String> {
        let rules = Rules::new(home, &[], &config.protected_paths(home))
            .map_err(|err| format!("resolving {}: {err}", home.display()))?;
        let mounts = Mounts::load()
            .map_err(|err| format!("reading the mount points from /proc/self/mountinfo: {err}"))?;
        Ok(Self::new(home, rules, mounts))
    }

    fn new(home: &Path, rules: Rules, mounts: Mounts) -> Self {
        let mut own: Vec<PathBuf> = [".config", ".local/share", ".local/state"]
            .iter()
            .map(|dir| home.join(dir).join("vole"))
            .collect();
        // Where the XDG variables may have moved them.
        own.extend(
            [dirs::config_dir(), dirs::data_dir(), dirs::state_dir()]
                .into_iter()
                .flatten()
                .map(|dir| dir.join("vole")),
        );
        let own = own
            .into_iter()
            .map(|dir| fs::canonicalize(&dir).unwrap_or(dir))
            .collect();
        Self {
            rules,
            mounts,
            trash: home.join(TRASH),
            own,
        }
    }

    /// Checks that `path` may be picked and moved to the trash, and returns where it really is.
    /// Else says why not, as words following the path: `is a sealed path`.
    pub fn check(&self, path: &Path) -> Result<PathBuf, String> {
        let real = self
            .rules
            .check_picked(path)
            .map_err(|refusal| format!("is {refusal}"))?;
        if self.mounts.contains(&real) {
            return Err("is a mount point".into());
        }
        // The trash would take the mounted disk along, and emptying it would delete its files.
        if self.mounts.any_inside(&real) {
            return Err("holds a mount point".into());
        }
        if real == self.trash {
            return Err("is the trash itself".into());
        }
        if real.starts_with(&self.trash) {
            return Err("is in the trash already".into());
        }
        if self.own.iter().any(|own| real.starts_with(own)) {
            return Err("is vole's own".into());
        }
        if self.own.iter().any(|own| own.starts_with(&real)) {
            return Err("holds vole's own files".into());
        }
        Ok(real)
    }

    /// Whether moving `path` would move a Sealed Path.
    pub fn is_sealed(&self, path: &Path) -> bool {
        self.rules.is_sealed(path)
    }

    pub fn is_protected(&self, path: &Path) -> bool {
        self.rules.is_protected(path)
    }
}

/// Flags the Items held by running programs now; flags set already stay.
pub fn mark_in_use<'a>(items: impl Iterator<Item = &'a mut Picked>) {
    let open = in_use::open_paths();
    for item in items {
        item.in_use |= in_use::is_in_use(&item.path, &open);
    }
}

/// Moves a path to the desktop trash: gio, or a stand-in in tests.
pub trait Trash {
    fn trash(&self, path: &Path) -> Result<(), String>;
}

pub struct Gio;

impl Trash for Gio {
    fn trash(&self, path: &Path) -> Result<(), String> {
        let output = Command::new(GIO)
            .args(["trash", "--"])
            .arg(path)
            .env("LC_ALL", "C")
            .stdin(Stdio::null())
            .output()
            .map_err(|err| format!("running {GIO}: {err}"))?;
        if output.status.success() {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(gio_error(&stderr).unwrap_or_else(|| format!("gio trash failed ({})", output.status)))
    }
}

/// gio's message, without the `gio: file:///…: ` in front.
fn gio_error(stderr: &str) -> Option<String> {
    let line = stderr
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())?;
    // The file's URI has its spaces escaped, so the first ": " after it ends it.
    let message = line
        .strip_prefix("gio: ")
        .and_then(|rest| rest.split_once(": "))
        .map_or(line, |(_, message)| message);
    Some(message.to_string())
}

/// Moves `items` to the trash with `trash`, each after a last check, and writes each attempt to
/// `log`. `progress` is told about each before it moves; once `stop` is set, the rest stay.
pub fn trash_all(
    items: &[Picked],
    picker: &Picker,
    trash: &dyn Trash,
    log: &mut Log,
    progress: &mut dyn FnMut(usize, usize, &str),
    stop: &AtomicBool,
) -> Vec<Outcome> {
    let mut outcomes = Vec::new();
    for (done, item) in items.iter().enumerate() {
        if stop.load(Ordering::Relaxed) {
            outcomes.push(Outcome {
                label: item.label.clone(),
                size: item.size,
                error: Some("stopped before moving it".into()),
                log_error: None,
                moved: None,
            });
            continue;
        }
        progress(done, items.len(), &item.label);
        let target = item.path.display().to_string();
        let (label, size) = (&item.label, item.size);
        outcomes.push(match trash_one(&item.path, picker, trash) {
            Ok(true) => log.record_trashed(label, KIND, size, &target, Ok(())),
            // Already gone: nothing was moved, nor freed by vole.
            Ok(false) => {
                let mut outcome = log.record(label, KIND, 0, &target, Ok(()));
                outcome.label.push_str(" (already gone)");
                outcome
            }
            Err(err) => log.record_trashed(label, KIND, size, &target, Err(err)),
        });
    }
    outcomes
}

/// Moves `path` to the trash after a last check. Returns whether it was still there.
fn trash_one(path: &Path, picker: &Picker, trash: &dyn Trash) -> Result<bool, String> {
    let real = picker
        .check(path)
        .map_err(|why| format!("refused: {} {why}", path.display()))?;
    match fs::symlink_metadata(&real) {
        Ok(_) => trash.trash(&real).map(|()| true),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(false),
        Err(err) => Err(format!("{}: {err}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::log::MovedTo;
    use crate::testdir::TestDir;

    struct Home {
        dir: TestDir,
        log: PathBuf,
    }

    fn home() -> Home {
        let dir = TestDir::new();
        Home {
            log: dir.path().join("log.jsonl"),
            dir,
        }
    }

    impl Home {
        fn picker(&self, mounts: Mounts) -> Picker {
            let rules = Rules::new(self.dir.path(), &[], &[]).unwrap();
            Picker::new(self.dir.path(), rules, mounts)
        }

        fn why(&self, relative: &str) -> Option<String> {
            self.picker(Mounts::default())
                .check(&self.dir.path().join(relative))
                .err()
        }
    }

    /// Moves what it is given into a folder of its own, as gio would into the trash.
    struct FakeTrash(PathBuf);

    impl Trash for FakeTrash {
        fn trash(&self, path: &Path) -> Result<(), String> {
            fs::create_dir_all(&self.0).map_err(|err| err.to_string())?;
            let name = path.file_name().ok_or("no name")?;
            fs::rename(path, self.0.join(name)).map_err(|err| err.to_string())
        }
    }

    fn picked(path: PathBuf) -> Picked {
        Picked {
            label: path.display().to_string(),
            path,
            size: 10,
            in_use: false,
        }
    }

    fn log_lines(path: &Path) -> Vec<serde_json::Value> {
        fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    #[test]
    fn picks_anything_in_home_but_what_vole_keeps() {
        let home = home();
        home.dir.file("Videos/old.mkv", 1);
        home.dir.dir("projects/app");
        home.dir.file(".local/share/Trash/files/a", 1);
        home.dir.file(".config/vole/config.toml", 1);
        home.dir.file(".local/state/vole/log.jsonl", 1);
        home.dir.dir(".ssh");
        assert_eq!(home.why("Videos/old.mkv"), None);
        assert_eq!(home.why("projects/app"), None);
        assert_eq!(home.why(".ssh").as_deref(), Some("is a sealed path"));
        assert_eq!(
            home.why(".local/share/Trash").as_deref(),
            Some("is the trash itself")
        );
        assert_eq!(
            home.why(".local/share/Trash/files/a").as_deref(),
            Some("is in the trash already")
        );
        assert_eq!(home.why(".config/vole").as_deref(), Some("is vole's own"));
        assert_eq!(
            home.why(".local/state").as_deref(),
            Some("holds vole's own files")
        );
        let picker = home.picker(Mounts::default());
        assert!(picker.is_sealed(&home.dir.path().join(".ssh")));
        assert!(picker.is_protected(&home.dir.path().join("projects/app")));
    }

    #[test]
    fn mount_points_and_what_holds_them_are_not_picked() {
        let home = home();
        let mounted = home.dir.dir("data/usb");
        let picker = home.picker(Mounts::from_paths([mounted.clone()]));
        assert_eq!(picker.check(&mounted).unwrap_err(), "is a mount point");
        assert_eq!(
            picker.check(&home.dir.path().join("data")).unwrap_err(),
            "holds a mount point"
        );
    }

    #[test]
    fn moves_items_to_the_trash_and_logs_them() {
        let home = home();
        let video = home.dir.file("Videos/old.mkv", 10);
        let folder = home.dir.dir("Downloads/iso");
        let trash = FakeTrash(home.dir.path().join("trash"));
        let mut log = Log::open(&home.log, "analyze").unwrap();
        let outcomes = trash_all(
            &[picked(video.clone()), picked(folder.clone())],
            &home.picker(Mounts::default()),
            &trash,
            &mut log,
            &mut |_, _, _| {},
            &AtomicBool::new(false),
        );
        assert!(
            outcomes
                .iter()
                .all(|o| o.error.is_none() && o.moved == Some(MovedTo::Trash))
        );
        assert!(!video.exists() && !folder.exists());
        assert!(trash.0.join("old.mkv").exists() && trash.0.join("iso").is_dir());
        let log = log_lines(&home.log);
        assert_eq!(log.len(), 2);
        assert_eq!(log[0]["module"], "analyze");
        assert_eq!(log[0]["kind"], "hand-picked");
        assert_eq!(log[0]["trashed"], true);
        assert_eq!(log[0]["target"], video.display().to_string());
    }

    #[test]
    fn refuses_what_became_sealed_and_skips_what_is_gone() {
        let home = home();
        let keys = home.dir.dir("keys");
        // The folder picked has since become the target of ~/.ssh.
        home.dir.symlink(".ssh", &keys);
        let gone = home.dir.path().join("gone.iso");
        let trash = FakeTrash(home.dir.path().join("trash"));
        let mut log = Log::open(&home.log, "analyze").unwrap();
        let outcomes = trash_all(
            &[picked(keys.clone()), picked(gone)],
            &home.picker(Mounts::default()),
            &trash,
            &mut log,
            &mut |_, _, _| {},
            &AtomicBool::new(false),
        );
        let error = outcomes[0].error.as_deref().unwrap();
        assert!(
            error.starts_with("refused") && error.ends_with("is a sealed path"),
            "{error}"
        );
        assert!(keys.exists());
        assert!(outcomes[1].error.is_none() && outcomes[1].label.ends_with("(already gone)"));
        assert_eq!(outcomes[1].size, 0);
        assert_eq!(log_lines(&home.log)[0]["trashed"], false);
    }

    #[test]
    fn stopping_leaves_the_rest() {
        let home = home();
        let first = home.dir.file("first", 10);
        let second = home.dir.file("second", 10);
        let trash = FakeTrash(home.dir.path().join("trash"));
        let mut log = Log::open(&home.log, "analyze").unwrap();
        let stop = AtomicBool::new(false);
        let mut seen = Vec::new();
        let outcomes = trash_all(
            &[picked(first.clone()), picked(second.clone())],
            &home.picker(Mounts::default()),
            &trash,
            &mut log,
            &mut |done, total, _| {
                seen.push((done, total));
                stop.store(true, Ordering::Relaxed);
            },
            &stop,
        );
        assert_eq!(seen, [(0, 2)]);
        assert!(!first.exists() && second.exists());
        assert_eq!(
            outcomes[1].error.as_deref(),
            Some("stopped before moving it")
        );
        assert_eq!(log_lines(&home.log).len(), 1);
    }

    #[test]
    fn reads_gios_errors() {
        assert_eq!(
            gio_error(
                "gio: file:///home/me/a%20b: Unable to trash file /home/me/a b: Permission denied\n"
            )
            .as_deref(),
            Some("Unable to trash file /home/me/a b: Permission denied")
        );
        assert_eq!(
            gio_error("something else").as_deref(),
            Some("something else")
        );
        assert_eq!(gio_error("\n"), None);
    }
}
