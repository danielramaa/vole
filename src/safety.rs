//! The path rules every module obeys: Allowed Roots, the Protected List and Sealed Paths.

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
    ".bitcoin",
    ".electrum",
    ".ethereum",
    ".monero",
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
        rules.sealed = rules.with_resolved(in_home(SEALED));
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
        self.check_with(path, true)
    }

    /// Checks that something vole removed may be put back at `path`: like [`Rules::check`], but
    /// the Protected List, which only keeps things from being proposed, doesn't stop it.
    pub fn check_put_back(&self, path: &Path) -> Result<PathBuf, Refusal> {
        self.check_with(path, false)
    }

    fn check_with(&self, path: &Path, protected: bool) -> Result<PathBuf, Refusal> {
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
        let loose_dotfile = self.loose_dotfiles
            && path.parent() == Some(self.home.as_path())
            && path
                .file_name()
                .is_some_and(|name| name.as_bytes().starts_with(b"."));
        if !(loose_dotfile || self.allowed_roots.iter().any(|root| path.starts_with(root))) {
            return Err(Refusal::OutsideAllowedRoots);
        }
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
