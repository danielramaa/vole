//! The path rules every module obeys: Allowed Roots, the Protected List and Sealed Paths.

use std::env;
use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};

/// Secrets and keys vole never removes by any means (relative to the home directory): keys,
/// password stores and keyrings, credentials, and wallets.
const SEALED: &[&str] = &[
    ".ssh",
    ".gnupg",
    ".password-store",
    ".pki",
    ".local/share/keyrings",
    ".local/share/kwalletd",
    ".netrc",
    ".git-credentials",
    ".pgpass",
    ".my.cnf",
    ".pypirc",
    ".vault-token",
    ".aws",
    ".azure",
    ".config/gcloud",
    ".kube",
    ".docker/config.json",
    ".terraform.d",
    ".cargo/credentials",
    ".cargo/credentials.toml",
    ".config/gh/hosts.yml",
    ".config/hub",
    ".config/rclone/rclone.conf",
    ".npmrc",
    ".gem/credentials",
    ".bitcoin",
    ".electrum",
    ".ethereum",
    ".monero",
];

/// Where environment variables keep secrets instead: the variable, and the secret in the folder
/// it names (empty for the folder itself).
const SEALED_ELSEWHERE: &[(&str, &str)] = &[
    ("GNUPGHOME", ""),
    ("PASSWORD_STORE_DIR", ""),
    ("XDG_DATA_HOME", "keyrings"),
    ("XDG_DATA_HOME", "kwalletd"),
    ("XDG_CONFIG_HOME", "gcloud"),
    ("XDG_CONFIG_HOME", "gh/hosts.yml"),
    ("XDG_CONFIG_HOME", "hub"),
    ("XDG_CONFIG_HOME", "rclone/rclone.conf"),
    ("GH_CONFIG_DIR", "hosts.yml"),
    ("CARGO_HOME", "credentials"),
    ("CARGO_HOME", "credentials.toml"),
    ("DOCKER_CONFIG", "config.json"),
];

/// Paths no module proposes on its own (relative to the home directory): browser and mail
/// profiles, and project folders. The user extends this with `protected_paths` in the config.
const PROTECTED: &[&str] = &[
    ".mozilla",
    ".thunderbird",
    ".librewolf",
    ".config/google-chrome",
    ".config/chromium",
    ".config/BraveSoftware",
    ".config/vivaldi",
    ".config/opera",
    "Projects",
    "projects",
    "src",
    "code",
    "dev",
    "git",
    "repos",
    "workspace",
];

/// Why a path may not be proposed or removed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// Relative, or contains `.` or `..`.
    NotNormalized,
    OutsideAllowedRoots,
    /// Not inside the home directory, where the Items picked by hand come from.
    OutsideHome,
    Protected,
    Sealed,
    /// A folder above the path is a symlink, so the path may really be somewhere else.
    Symlinked,
}

impl fmt::Display for Refusal {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(match self {
            Refusal::NotNormalized => "not a plain absolute path",
            Refusal::OutsideAllowedRoots => "outside the allowed roots",
            Refusal::OutsideHome => "outside the home folder",
            Refusal::Protected => "on the protected list",
            Refusal::Sealed => "a sealed path",
            Refusal::Symlinked => "reached through a symlink",
        })
    }
}

pub struct Rules {
    home: PathBuf,
    /// The home directory with every symlink resolved.
    real_home: PathBuf,
    allowed_roots: Vec<PathBuf>,
    /// Whether the dotfiles directly in the home directory count as inside the Allowed Roots.
    loose_dotfiles: bool,
    /// Each rule path both as written and as resolved on disk, so a symlink can't hide it.
    protected: Vec<PathBuf>,
    sealed: Vec<PathBuf>,
}

impl Rules {
    /// `allowed_roots` are relative to `home`; `extra_protected` are absolute.
    pub fn new(
        home: &Path,
        allowed_roots: &[&str],
        extra_protected: &[PathBuf],
    ) -> io::Result<Self> {
        let real_home = fs::canonicalize(home)?;
        let in_home =
            |relative: &[&str]| -> Vec<PathBuf> { relative.iter().map(|r| home.join(r)).collect() };
        let mut protected = in_home(PROTECTED);
        protected.extend_from_slice(extra_protected);
        let mut rules = Self {
            home: home.to_path_buf(),
            real_home,
            allowed_roots: in_home(allowed_roots),
            loose_dotfiles: false,
            protected: Vec::new(),
            sealed: Vec::new(),
        };
        rules.protected = rules.with_resolved(protected);
        let mut sealed = in_home(SEALED);
        sealed.extend(sealed_elsewhere(&|name| env::var_os(name)));
        rules.sealed = rules.with_resolved(sealed);
        Ok(rules)
    }

    /// Also allows the dotfiles and dotfolders directly in the home directory (`~/.foorc`), as
    /// if each were an Allowed Root. The other rules still apply: `~/.ssh` stays sealed.
    pub fn with_loose_dotfiles(mut self) -> Self {
        self.loose_dotfiles = true;
        self
    }

    fn with_resolved(&self, paths: Vec<PathBuf>) -> Vec<PathBuf> {
        let mut all = Vec::new();
        for path in paths {
            all.extend(fs::canonicalize(&path).ok());
            if let Ok(relative) = path.strip_prefix(&self.home) {
                all.push(self.real_home.join(relative));
            }
            all.push(path);
        }
        all
    }

    /// Checks that `path` may be proposed (and, right before removal, removed) by a module, and
    /// returns where it really is (see [`Rules::resolve`]). `path` itself may be a symlink:
    /// removing it only removes the link.
    pub fn check(&self, path: &Path) -> Result<PathBuf, Refusal> {
        self.check_with(path, Purpose::Propose)
    }

    /// Checks that something vole removed may be put back at `path`: like [`Rules::check`], but
    /// the Protected List, which only keeps things from being proposed, doesn't stop it.
    pub fn check_put_back(&self, path: &Path) -> Result<PathBuf, Refusal> {
        self.check_with(path, Purpose::PutBack)
    }

    /// Checks that the user may pick `path` by hand in the disk explorer, and returns where it
    /// really is: anywhere inside the home directory, the Protected List included, but never a
    /// Sealed Path, nor through a symlinked folder.
    pub fn check_picked(&self, path: &Path) -> Result<PathBuf, Refusal> {
        self.check_with(path, Purpose::Pick)
    }

    /// Whether removing `path` would remove a Sealed Path: it is one, is inside one, or holds one.
    pub fn is_sealed(&self, path: &Path) -> bool {
        overlaps(path, &self.sealed)
    }

    /// Whether `path` is on the Protected List or inside a path that is.
    pub fn is_protected(&self, path: &Path) -> bool {
        self.protected.iter().any(|rule| path.starts_with(rule))
    }

    fn check_with(&self, path: &Path, purpose: Purpose) -> Result<PathBuf, Refusal> {
        if !path.is_absolute()
            || path
                .components()
                .any(|c| matches!(c, Component::CurDir | Component::ParentDir))
        {
            return Err(Refusal::NotNormalized);
        }
        if overlaps(path, &self.sealed) {
            return Err(Refusal::Sealed);
        }
        if purpose == Purpose::Pick {
            if !path.starts_with(&self.home) || path == self.home {
                return Err(Refusal::OutsideHome);
            }
        } else if !self.in_allowed_roots(path) {
            return Err(Refusal::OutsideAllowedRoots);
        }
        let protected = purpose == Purpose::Propose;
        if protected && overlaps(path, &self.protected) {
            return Err(Refusal::Protected);
        }
        let real = self.resolve(path).ok_or(Refusal::Symlinked)?;
        if overlaps(&real, &self.sealed) {
            return Err(Refusal::Sealed);
        }
        if protected && overlaps(&real, &self.protected) {
            return Err(Refusal::Protected);
        }
        Ok(real)
    }

    fn in_allowed_roots(&self, path: &Path) -> bool {
        let loose_dotfile = self.loose_dotfiles
            && path.parent() == Some(self.home.as_path())
            && path
                .file_name()
                .is_some_and(|name| name.as_bytes().starts_with(b"."));
        loose_dotfile || self.allowed_roots.iter().any(|root| path.starts_with(root))
    }

    /// Where `path` (inside the home directory) really is, without following `path` itself.
    /// `None` when a folder between the home directory and `path` is a symlink, or is missing.
    pub fn resolve(&self, path: &Path) -> Option<PathBuf> {
        let parent = path.parent()?;
        let expected = self.real_home.join(parent.strip_prefix(&self.home).ok()?);
        if fs::canonicalize(parent).ok()? != expected {
            return None;
        }
        Some(expected.join(path.file_name()?))
    }
}

/// The secrets environment variables keep elsewhere, as `env` says (see [`SEALED_ELSEWHERE`]).
/// Only absolute paths count.
fn sealed_elsewhere(env: &dyn Fn(&str) -> Option<OsString>) -> Vec<PathBuf> {
    SEALED_ELSEWHERE
        .iter()
        .filter_map(|&(name, secret)| {
            let folder = PathBuf::from(env(name)?);
            let path = if secret.is_empty() {
                folder
            } else {
                folder.join(secret)
            };
            path.is_absolute().then_some(path)
        })
        .collect()
}

/// What a path is checked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Purpose {
    /// Proposed by a module, or removed after it was.
    Propose,
    /// Put back where vole took it from.
    PutBack,
    /// Picked by hand in the disk explorer.
    Pick,
}

/// Whether `path` is one of `rules`, inside one, or contains one (removing it would remove the rule path).
fn overlaps(path: &Path, rules: &[PathBuf]) -> bool {
    rules
        .iter()
        .any(|rule| path.starts_with(rule) || rule.starts_with(path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testdir::TestDir;

    fn rules(home: &TestDir, extra_protected: &[PathBuf]) -> Rules {
        Rules::new(home.path(), &[".cache", ".local/share"], extra_protected).unwrap()
    }

    #[test]
    fn allows_paths_under_a_root() {
        let home = TestDir::new();
        let path = home.dir(".cache/foo");
        assert_eq!(rules(&home, &[]).check(&path), Ok(path.clone()));
        // The root itself, and paths that don't exist yet.
        let cache = home.path().join(".cache");
        assert_eq!(rules(&home, &[]).check(&cache), Ok(cache));
        let missing = path.join("missing");
        assert_eq!(rules(&home, &[]).check(&missing), Ok(missing));
    }

    #[test]
    fn refuses_paths_outside_the_roots() {
        let home = TestDir::new();
        let rules = rules(&home, &[]);
        assert_eq!(
            rules.check(&home.dir("Documents")),
            Err(Refusal::OutsideAllowedRoots)
        );
        // The home directory holds the sealed paths, and that takes precedence.
        assert_eq!(rules.check(home.path()), Err(Refusal::Sealed));
        assert_eq!(
            rules.check(Path::new("/etc")),
            Err(Refusal::OutsideAllowedRoots)
        );
        // Looks like the root, but isn't inside it.
        assert_eq!(
            rules.check(&home.dir(".cache2")),
            Err(Refusal::OutsideAllowedRoots)
        );
    }

    #[test]
    fn refuses_relative_and_dotted_paths() {
        let home = TestDir::new();
        let rules = rules(&home, &[]);
        home.dir(".cache");
        assert_eq!(
            rules.check(Path::new(".cache/foo")),
            Err(Refusal::NotNormalized)
        );
        let escape = home.path().join(".cache/../Documents");
        assert_eq!(rules.check(&escape), Err(Refusal::NotNormalized));
    }

    #[test]
    fn may_allow_dotfiles_directly_in_home() {
        let home = TestDir::new();
        let rules = rules(&home, &[]).with_loose_dotfiles();
        let dotfile = home.file(".foorc", 1);
        assert_eq!(rules.check(&dotfile), Ok(dotfile));
        let dotfolder = home.dir(".foo");
        assert_eq!(rules.check(&dotfolder), Ok(dotfolder.clone()));
        // Only directly in home, and the other rules still apply.
        assert_eq!(
            rules.check(&home.file(".foo/bar", 1)),
            Err(Refusal::OutsideAllowedRoots)
        );
        assert_eq!(
            rules.check(&home.dir("Documents")),
            Err(Refusal::OutsideAllowedRoots)
        );
        assert_eq!(rules.check(&home.dir(".ssh")), Err(Refusal::Sealed));
        assert_eq!(
            rules.check(&home.path().join(".local")),
            Err(Refusal::Sealed)
        );
        // Without it, a dotfile is outside the roots.
        assert_eq!(
            Rules::new(home.path(), &[".cache"], &[])
                .unwrap()
                .check(&home.path().join(".foorc")),
            Err(Refusal::OutsideAllowedRoots)
        );
    }

    #[test]
    fn credentials_and_wallets_are_sealed() {
        let home = TestDir::new();
        let rules = rules(&home, &[]).with_loose_dotfiles();
        for path in [".pgpass", ".aws", ".bitcoin", ".netrc"] {
            assert_eq!(
                rules.check(&home.path().join(path)),
                Err(Refusal::Sealed),
                "{path}"
            );
        }
        // A folder holding credentials goes with them.
        home.file(".docker/config.json", 1);
        assert_eq!(
            rules.check(&home.path().join(".docker")),
            Err(Refusal::Sealed)
        );
    }

    #[test]
    fn tokens_of_developer_tools_are_sealed() {
        let home = TestDir::new();
        let rules = rules(&home, &[]).with_loose_dotfiles();
        for path in [".npmrc", ".cargo/credentials.toml", ".config/gh/hosts.yml"] {
            assert_eq!(
                rules.check_picked(&home.path().join(path)),
                Err(Refusal::Sealed),
                "{path}"
            );
        }
        // Their folders go with them; the rest of the cargo folder stays a cache.
        assert_eq!(
            rules.check_picked(&home.path().join(".config/gh")),
            Err(Refusal::Sealed)
        );
        let registry = home.dir(".cargo/registry");
        assert_eq!(rules.check_picked(&registry), Ok(registry));
    }

    #[test]
    fn seals_secrets_where_the_environment_keeps_them() {
        let env = |name: &str| -> Option<OsString> {
            match name {
                "GNUPGHOME" => Some("/h/.local/share/gnupg".into()),
                "XDG_DATA_HOME" => Some("/h/.data".into()),
                "CARGO_HOME" => Some("relative/cargo".into()),
                _ => None,
            }
        };
        let sealed = sealed_elsewhere(&env);
        assert_eq!(
            sealed,
            [
                PathBuf::from("/h/.local/share/gnupg"),
                PathBuf::from("/h/.data/keyrings"),
                PathBuf::from("/h/.data/kwalletd"),
            ]
        );
    }

    #[test]
    fn refuses_sealed_paths_and_what_contains_them() {
        let home = TestDir::new();
        let keyrings = home.dir(".local/share/keyrings");
        let rules = rules(&home, &[]);
        assert_eq!(rules.check(&keyrings), Err(Refusal::Sealed));
        assert_eq!(
            rules.check(&keyrings.join("login.keyring")),
            Err(Refusal::Sealed)
        );
        assert_eq!(
            rules.check(&home.path().join(".local/share")),
            Err(Refusal::Sealed)
        );
    }

    #[test]
    fn refuses_protected_paths_and_what_contains_them() {
        let home = TestDir::new();
        let kept = home.dir(".cache/keep/inner");
        let rules = rules(&home, &[home.path().join(".cache/keep")]);
        assert_eq!(rules.check(&kept), Err(Refusal::Protected));
        assert_eq!(
            rules.check(&home.path().join(".cache")),
            Err(Refusal::Protected)
        );
        let other = home.dir(".cache/other");
        assert_eq!(rules.check(&other), Ok(other));
    }

    #[test]
    fn putting_back_ignores_only_the_protected_list() {
        let home = TestDir::new();
        let kept = home.path().join(".cache/keep");
        let rules = rules(&home, std::slice::from_ref(&kept));
        home.dir(".cache");
        assert_eq!(rules.check(&kept), Err(Refusal::Protected));
        assert_eq!(rules.check_put_back(&kept), Ok(kept));
        assert_eq!(
            rules.check_put_back(&home.dir(".local/share/keyrings")),
            Err(Refusal::Sealed)
        );
    }

    #[test]
    fn picking_reaches_the_whole_home_but_sealed_paths() {
        let home = TestDir::new();
        let kept = home.dir(".cache/keep");
        let rules = rules(&home, std::slice::from_ref(&kept));
        let video = home.file("Videos/old.mkv", 1);
        assert_eq!(rules.check_picked(&video), Ok(video.clone()));
        // The Protected List only keeps modules from proposing things.
        assert_eq!(rules.check(&kept), Err(Refusal::Protected));
        assert_eq!(rules.check_picked(&kept), Ok(kept.clone()));
        let project = home.dir("projects/app");
        assert_eq!(rules.check_picked(&project), Ok(project.clone()));
        assert!(rules.is_protected(&project) && rules.is_protected(&kept));
        assert!(!rules.is_protected(&video));
        assert_eq!(rules.check_picked(&home.dir(".ssh")), Err(Refusal::Sealed));
        // A folder holding a sealed path would take it along.
        assert_eq!(
            rules.check_picked(&home.path().join(".local")),
            Err(Refusal::Sealed)
        );
        assert!(rules.is_sealed(&home.path().join(".local")));
        assert_eq!(
            rules.check_picked(Path::new("/etc/passwd")),
            Err(Refusal::OutsideHome)
        );
        let outside = home.path().with_extension("other");
        assert_eq!(rules.check_picked(&outside), Err(Refusal::OutsideHome));
    }

    #[test]
    fn picking_never_takes_the_home_folder_itself_or_goes_through_symlinks() {
        let home = TestDir::new();
        let outside = TestDir::new();
        outside.file("important", 1);
        home.symlink("link", outside.path());
        let rules = rules(&home, &[]);
        assert_eq!(rules.check_picked(home.path()), Err(Refusal::Sealed));
        assert_eq!(
            rules.check_picked(&home.path().join("link/important")),
            Err(Refusal::Symlinked)
        );
        // The link itself can go: only the link moves.
        let link = home.path().join("link");
        assert_eq!(rules.check_picked(&link), Ok(link));
    }

    #[test]
    fn refuses_paths_behind_a_symlinked_folder() {
        let home = TestDir::new();
        let outside = TestDir::new();
        outside.file("important/file", 1);
        home.symlink(".cache/link", outside.path());
        let rules = rules(&home, &[]);
        let escape = home.path().join(".cache/link/important");
        assert_eq!(rules.check(&escape), Err(Refusal::Symlinked));
        // The link itself can go: removing it doesn't touch what it points to.
        let link = home.path().join(".cache/link");
        assert_eq!(rules.check(&link), Ok(link));
    }

    #[test]
    fn refuses_a_symlinked_root() {
        let home = TestDir::new();
        let documents = home.dir("Documents");
        home.file("Documents/thesis.pdf", 1);
        home.symlink(".cache", &documents);
        let path = home.path().join(".cache/thesis.pdf");
        assert_eq!(rules(&home, &[]).check(&path), Err(Refusal::Symlinked));
    }

    #[test]
    fn refuses_the_target_of_a_symlinked_sealed_path() {
        let home = TestDir::new();
        let keys = home.dir(".cache/keys");
        home.symlink(".ssh", &keys);
        assert_eq!(rules(&home, &[]).check(&keys), Err(Refusal::Sealed));
    }
}
