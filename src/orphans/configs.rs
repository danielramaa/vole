//! Orphaned Configs: files and folders in the home directory that no Owner accounts for.

use std::cmp::Reverse;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use super::owners::Owners;
use crate::disk::{self, Mounts};
use crate::safety::Rules;
use crate::{date, in_use};

/// Where Orphaned Configs are looked for, relative to the home directory, besides the dotfiles
/// directly in it.
pub const ROOTS: &[&str] = &[".config", ".local/share", ".local/state", ".cache"];

/// Changed this many days ago or later, a config may still be in use: Medium, not High.
const RECENT_DAYS: u64 = 90;

/// How likely an Orphaned Config really has no Owner. Sorts High first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Confidence {
    High,
    Medium,
    Low,
}

impl Confidence {
    pub fn name(self) -> &'static str {
        match self {
            Confidence::High => "High",
            Confidence::Medium => "Medium",
            Confidence::Low => "Low",
        }
    }
}

/// A file or folder that may be an Orphaned Config.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub path: PathBuf,
    /// Where it really is, the folders above it resolved.
    pub real: PathBuf,
    /// A dotfile directly in the home directory, like `~/.foorc`.
    pub loose: bool,
}

impl Candidate {
    pub fn name(&self) -> String {
        self.path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned()
    }
}

#[derive(Debug, Clone)]
pub struct OrphanedConfig {
    pub path: PathBuf,
    /// The path as shown, starting with `~/`.
    pub label: String,
    pub size: u64,
    pub confidence: Confidence,
    /// Why it has its Confidence, e.g. `unchanged since Mar 2025`.
    pub reason: String,
    /// Held by a running process.
    pub in_use: bool,
}

/// The files and folders directly in the [`ROOTS`] and the dotfiles directly in `home` that the
/// path rules allow, not following symlinks; also how many the rules left out.
pub fn candidates(home: &Path, rules: &Rules, mounts: &Mounts) -> (Vec<Candidate>, usize) {
    let mut found = Vec::new();
    let mut refused = 0;
    let mut look_in = |dir: &Path, loose: bool| {
        let mut entries: Vec<PathBuf> = fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|entry| entry.file_type().is_ok_and(|t| !t.is_symlink()))
            .map(|entry| entry.path())
            .filter(|path| !loose || is_loose_dotfile(home, path))
            .collect();
        entries.sort();
        for path in entries {
            // A mount point is not the config's to move, even when it holds the same filesystem.
            match rules.check(&path) {
                Ok(real) if !mounts.contains(&real) => found.push(Candidate { path, real, loose }),
                _ => refused += 1,
            }
        }
    };
    for root in ROOTS {
        look_in(&home.join(root), false);
    }
    look_in(home, true);
    (found, refused)
}

/// Whether `path` is a dotfile directly in `home`, and not one of the folders holding the
/// [`ROOTS`] (`~/.config`, `~/.local`, `~/.cache`).
fn is_loose_dotfile(home: &Path, path: &Path) -> bool {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    path.parent() == Some(home)
        && name.starts_with('.')
        && !ROOTS
            .iter()
            .any(|root| root.split('/').next() == Some(name.as_ref()))
}

/// The `candidates` no Owner accounts for, measured, High Confidence first and largest first
/// within each.
pub fn find(
    home: &Path,
    candidates: &[Candidate],
    owners: &Owners,
    mounts: &Mounts,
    now: SystemTime,
) -> Vec<OrphanedConfig> {
    let mut orphans = Vec::new();
    for candidate in candidates {
        let name = candidate.name();
        if !owners.of(&candidate.real, &name).is_empty() {
            continue;
        }
        let usage = disk::usage(&candidate.path, mounts);
        let (confidence, reason) =
            confidence(candidate.loose, owners.close_to(&name), usage.newest, now);
        let relative = candidate.path.strip_prefix(home).unwrap_or(&candidate.path);
        orphans.push(OrphanedConfig {
            path: candidate.path.clone(),
            label: format!("~/{}", relative.display()),
            size: usage.bytes,
            confidence,
            reason,
            in_use: false,
        });
    }
    orphans.sort_by_key(|orphan| (orphan.confidence, Reverse(orphan.size)));
    orphans
}

/// Flags the configs held by a running program; `open` is from [`in_use::open_paths`].
pub fn mark_in_use(configs: &mut [OrphanedConfig], rules: &Rules, open: &[PathBuf]) {
    for config in configs {
        config.in_use |= rules
            .resolve(&config.path)
            .is_some_and(|real| in_use::is_in_use(&real, open));
    }
}

/// How sure vole is that nothing owns a config: dotfiles directly in the home directory are too
/// varied to tell; a name like an installed package's, or a recent change, makes it less sure.
fn confidence(
    loose: bool,
    close_to: Option<String>,
    newest: Option<SystemTime>,
    now: SystemTime,
) -> (Confidence, String) {
    if loose {
        return (Confidence::Low, "dotfile directly in ~".into());
    }
    if let Some(name) = close_to {
        return (Confidence::Medium, format!("name like {name}"));
    }
    let Some(newest) = newest else {
        return (Confidence::Medium, "can't tell when it last changed".into());
    };
    match date::days_between(newest, now) {
        0 => (Confidence::Medium, "changed today".into()),
        1 => (Confidence::Medium, "changed yesterday".into()),
        days if days < RECENT_DAYS => (Confidence::Medium, format!("changed {days} days ago")),
        _ => (
            Confidence::High,
            format!("unchanged since {}", date::month(newest)),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orphans::owners::{self, Owners};
    use crate::orphans::pacman::Info;
    use crate::testdir::TestDir;
    use std::time::Duration;

    const DAY: Duration = Duration::from_secs(86_400);

    fn rules(home: &TestDir) -> Rules {
        Rules::new(home.path(), ROOTS, &[])
            .unwrap()
            .with_loose_dotfiles()
    }

    fn labels(home: &TestDir, candidates: &[Candidate]) -> Vec<String> {
        candidates
            .iter()
            .map(|c| {
                let relative = c.path.strip_prefix(home.path()).unwrap().display();
                format!("{relative}{}", if c.loose { " (loose)" } else { "" })
            })
            .collect()
    }

    #[test]
    fn candidates_are_in_the_roots_and_dotfiles_in_home() {
        let home = TestDir::new();
        home.dir(".config/app");
        home.file(".config/apprc", 1);
        home.dir(".local/share/game");
        home.file(".local/state/app.log", 1);
        home.dir(".cache/app");
        home.file(".foorc", 1);
        home.dir(".tool");
        home.dir("Documents");
        home.file("notes.txt", 1);
        // Symlinks are left alone: whoever made one manages it.
        home.symlink(".config/linked", &home.dir("dotfiles/linked"));
        home.symlink(".linkedrc", &home.file("dotfiles/rc", 1));
        let (found, refused) = candidates(home.path(), &rules(&home), &Mounts::default());
        assert_eq!(
            labels(&home, &found),
            [
                ".config/app",
                ".config/apprc",
                ".local/share/game",
                ".local/state/app.log",
                ".cache/app",
                ".foorc (loose)",
                ".tool (loose)",
            ]
        );
        assert_eq!(refused, 0);
    }

    #[test]
    fn candidates_leave_out_what_the_rules_refuse() {
        let home = TestDir::new();
        home.dir(".ssh");
        home.dir(".gnupg");
        home.dir(".mozilla");
        home.dir(".local/share/keyrings");
        home.dir(".config/kept");
        home.dir(".config/app");
        let mounted = home.dir(".cache/mounted");
        let rules = Rules::new(home.path(), ROOTS, &[home.path().join(".config/kept")])
            .unwrap()
            .with_loose_dotfiles();
        let mounts = Mounts::from_paths([mounted]);
        let (found, refused) = candidates(home.path(), &rules, &mounts);
        assert_eq!(labels(&home, &found), [".config/app"]);
        // .ssh, .gnupg, .mozilla, keyrings, kept and the mount point.
        assert_eq!(refused, 6);
    }

    fn installed(names: &[&str]) -> Vec<Info> {
        names
            .iter()
            .map(|name| Info {
                name: name.to_string(),
                ..Info::default()
            })
            .collect()
    }

    fn set_changed(path: &Path, time: SystemTime) {
        fs::File::open(path).unwrap().set_modified(time).unwrap();
    }

    #[test]
    fn finds_the_configs_nothing_owns_with_their_confidence() {
        let home = TestDir::new();
        let now = SystemTime::now();
        let old = now - 400 * DAY;
        for (path, size, changed) in [
            (".config/htop/htoprc", 10, old),
            (".config/gone/settings", 10, old),
            (".local/share/big-gone/data", 100_000, old),
            (".cache/recent-gone/data", 10, now - 3 * DAY),
            (".local/share/systemmonitor/data", 10, old),
            (".gonerc", 10, old),
        ] {
            let file = home.file(path, size);
            set_changed(&file, changed);
            set_changed(file.parent().unwrap(), changed);
        }
        let (candidates, _) = candidates(home.path(), &rules(&home), &Mounts::default());
        let names: Vec<String> = candidates.iter().map(Candidate::name).collect();
        let owners = Owners::from_pacman(
            &installed(&["htop", "plasma-systemmonitor"]),
            "",
            &owners::wanted(names.iter().map(String::as_str)),
        );
        let found = find(home.path(), &candidates, &owners, &Mounts::default(), now);
        let summary: Vec<(String, Confidence, String)> = found
            .iter()
            .map(|o| (o.label.clone(), o.confidence, o.reason.clone()))
            .collect();
        let month = date::month(old);
        assert_eq!(
            summary,
            [
                (
                    "~/.local/share/big-gone".to_string(),
                    Confidence::High,
                    format!("unchanged since {month}")
                ),
                (
                    "~/.config/gone".to_string(),
                    Confidence::High,
                    format!("unchanged since {month}")
                ),
                (
                    "~/.local/share/systemmonitor".to_string(),
                    Confidence::Medium,
                    "name like plasma-systemmonitor".to_string()
                ),
                (
                    "~/.cache/recent-gone".to_string(),
                    Confidence::Medium,
                    "changed 3 days ago".to_string()
                ),
                (
                    "~/.gonerc".to_string(),
                    Confidence::Low,
                    "dotfile directly in ~".to_string()
                ),
            ]
        );
        assert!(found[0].size > found[1].size);
    }

    #[test]
    fn marks_the_configs_a_program_holds() {
        let home = TestDir::new();
        let held = home.dir(".config/held");
        let config = |path: PathBuf| OrphanedConfig {
            label: String::new(),
            size: 0,
            confidence: Confidence::High,
            reason: String::new(),
            in_use: false,
            path,
        };
        let mut configs = [config(held.clone()), config(home.dir(".config/free"))];
        mark_in_use(&mut configs, &rules(&home), &[held.join("db.sqlite")]);
        assert!(configs[0].in_use && !configs[1].in_use);
    }

    #[test]
    fn says_how_recently_a_config_changed() {
        let now = SystemTime::now();
        let at = |days| Some(now - days * DAY);
        let reason = |newest| confidence(false, None, newest, now);
        assert_eq!(reason(at(0)), (Confidence::Medium, "changed today".into()));
        assert_eq!(
            reason(at(1)),
            (Confidence::Medium, "changed yesterday".into())
        );
        assert_eq!(reason(at(89)).0, Confidence::Medium);
        assert_eq!(reason(at(90)).0, Confidence::High);
        assert_eq!(reason(None).0, Confidence::Medium);
        // A dotfile in ~ is Low whatever else is true.
        assert_eq!(confidence(true, None, at(400), now).0, Confidence::Low);
    }
}
