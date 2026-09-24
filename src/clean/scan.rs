//! The Categories inside the home directory, measured by walking the disk.

use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::fs::{self, Metadata};
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use super::plan::{Category, Item, Kind};
use crate::safety::Rules;

/// Where `clean` may propose Items on its own (relative to the home directory).
pub const ALLOWED_ROOTS: &[&str] = &[
    ".cache",
    ".local/share/Trash",
    ".npm",
    ".cargo/registry",
    ".cargo/git",
    "go/pkg/mod",
];

const YAY: &str = ".cache/yay";

const DEVELOPER: &[(&str, &str)] = &[
    ("pip", ".cache/pip"),
    ("npm", ".npm/_cacache"),
    ("cargo registry", ".cargo/registry/cache"),
    ("cargo sources", ".cargo/registry/src"),
    ("cargo git", ".cargo/git/db"),
    ("cargo git checkouts", ".cargo/git/checkouts"),
    ("go modules", "go/pkg/mod"),
    ("go build", ".cache/go-build"),
];

const USER_CACHE: &str = ".cache";

const TRASH: &str = ".local/share/Trash";
/// The folders inside the trash that hold trashed files; the trash folder itself stays.
pub const TRASH_PARTS: &[&str] = &["files", "info", "expunged"];

/// The home Categories, in the order they claim their paths.
pub struct HomeCategories {
    pub yay: Category,
    pub developer: Category,
    pub trash: Category,
    pub user_cache: Category,
}

/// Paths already taken by a Category. A path overlapping a claimed one can't be claimed again,
/// so no byte is counted twice.
#[derive(Default)]
pub struct Claims(Vec<PathBuf>);

impl Claims {
    pub fn claim(&mut self, path: &Path) -> bool {
        if self
            .0
            .iter()
            .any(|claimed| claimed.starts_with(path) || path.starts_with(claimed))
        {
            return false;
        }
        self.0.push(path.to_path_buf());
        true
    }
}

/// Builds the home Categories. Specific Categories claim their paths before the general
/// user cache, which gets whatever is left in `~/.cache`.
pub fn home_categories(home: &Path, rules: &Rules, mounts: &Mounts) -> HomeCategories {
    let mut claims = Claims::default();

    let mut yay = Builder::new("yay cache", rules, mounts);
    let path = home.join(YAY);
    claims.claim(&path);
    yay.add_regenerable(format!("~/{YAY}"), path);

    let mut developer = Builder::new("Developer caches", rules, mounts);
    for (name, relative) in DEVELOPER {
        let path = home.join(relative);
        claims.claim(&path);
        developer.add_regenerable(format!("{name} (~/{relative})"), path);
    }

    let mut trash = Builder::new("Trash", rules, mounts);
    let path = home.join(TRASH);
    claims.claim(&path);
    trash.add_trash(path);

    let mut user_cache = Builder::new("User cache", rules, mounts);
    let mut folders: Vec<PathBuf> = fs::read_dir(home.join(USER_CACHE))
        .into_iter()
        .flatten()
        .flatten()
        // Not following symlinks: a symlinked folder is not part of the cache.
        .filter(|entry| entry.file_type().is_ok_and(|t| t.is_dir()))
        .map(|entry| entry.path())
        .collect();
    folders.sort();
    for path in folders {
        if claims.claim(&path) {
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            user_cache.add_regenerable(format!("~/{USER_CACHE}/{name}"), path.clone());
        }
    }
    let mut user_cache = user_cache.finish();
    user_cache.items.sort_by(|a, b| b.size.cmp(&a.size));

    HomeCategories {
        yay: yay.finish(),
        developer: developer.finish(),
        trash: trash.finish(),
        user_cache,
    }
}

struct Builder<'a> {
    category: Category,
    rules: &'a Rules,
    mounts: &'a Mounts,
    refused: usize,
}

impl<'a> Builder<'a> {
    fn new(name: &'static str, rules: &'a Rules, mounts: &'a Mounts) -> Self {
        Self {
            category: Category::new(name),
            rules,
            mounts,
            refused: 0,
        }
    }

    /// Whether `path` exists and the path rules allow proposing it.
    fn allowed(&mut self, path: &Path) -> bool {
        if fs::symlink_metadata(path).is_err() {
            return false;
        }
        // A mount point is not the cache's to remove, even when it holds the same filesystem.
        let allowed = self.rules.check(path).is_ok()
            && !self
                .rules
                .resolve(path)
                .is_some_and(|real| self.mounts.contains(&real));
        if !allowed {
            self.refused += 1;
        }
        allowed
    }

    fn add_regenerable(&mut self, label: String, path: PathBuf) {
        if self.allowed(&path) {
            let size = disk_usage(&path, self.mounts);
            self.push(label, Kind::Regenerable(path), size);
        }
    }

    fn add_trash(&mut self, path: PathBuf) {
        if !self.allowed(&path) {
            return;
        }
        // Emptying works on what is inside, so unlike other Items the trash folder itself
        // must not be a symlink.
        if !is_real_dir(&path) {
            self.refused += 1;
            return;
        }
        let size = trash_parts(&path, self.rules)
            .iter()
            .flat_map(|part| fs::read_dir(part).into_iter().flatten().flatten())
            .map(|entry| disk_usage(&entry.path(), self.mounts))
            .sum();
        self.push("empty the trash".into(), Kind::Trash(path), size);
    }

    fn push(&mut self, label: String, kind: Kind, size: u64) {
        if size > 0 {
            self.category.items.push(Item::new(label, kind, size));
        }
    }

    fn finish(mut self) -> Category {
        if self.refused > 0 {
            self.category.note = Some(format!(
                "{} path(s) left out by the safety rules",
                self.refused
            ));
        }
        self.category
    }
}

/// The folders of the trash whose contents may be removed: real folders (not symlinks) that the
/// path rules allow.
pub fn trash_parts(trash: &Path, rules: &Rules) -> Vec<PathBuf> {
    TRASH_PARTS
        .iter()
        .map(|part| trash.join(part))
        .filter(|part| is_real_dir(part) && rules.check(part).is_ok())
        .collect()
}

fn is_real_dir(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|meta| meta.is_dir())
}

/// Every mount point, bind mounts included, from `/proc/self/mountinfo`.
#[derive(Default)]
pub struct Mounts(HashSet<PathBuf>);

impl Mounts {
    pub fn load() -> Self {
        fs::read_to_string("/proc/self/mountinfo")
            .map(|text| Self::parse(&text))
            .unwrap_or_default()
    }

    fn parse(mountinfo: &str) -> Self {
        Self(
            mountinfo
                .lines()
                .filter_map(|line| line.split(' ').nth(4))
                .map(unescape_octal)
                .collect(),
        )
    }

    pub fn contains(&self, path: &Path) -> bool {
        self.0.contains(path)
    }
}

/// Decodes the `\040`-style escapes mountinfo uses for spaces and other special bytes.
fn unescape_octal(field: &str) -> PathBuf {
    let bytes = field.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let code = bytes.get(i + 1..i + 4).and_then(|digits| {
            let digits = std::str::from_utf8(digits).ok()?;
            u8::from_str_radix(digits, 8).ok()
        });
        match code {
            Some(code) if bytes[i] == b'\\' => {
                out.push(code);
                i += 4;
            }
            _ => {
                out.push(bytes[i]);
                i += 1;
            }
        }
    }
    PathBuf::from(OsString::from_vec(out))
}

/// Space freed by removing `path`: allocated blocks, not following symlinks, not crossing into
/// other filesystems or mount points. A hard-linked file only counts when all of its links are inside `path`,
/// since otherwise removing `path` doesn't free it.
pub fn disk_usage(path: &Path, mounts: &Mounts) -> u64 {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return 0;
    };
    // Walk the real path, so it can be compared with the mount points.
    let path = if meta.is_symlink() {
        path.to_path_buf()
    } else {
        fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
    };
    let device = meta.dev();
    // Inode → (links seen, links in total, bytes).
    let mut linked: HashMap<u64, (u64, u64, u64)> = HashMap::new();
    let mut total = 0;
    let mut stack: Vec<(PathBuf, Metadata)> = vec![(path, meta)];
    while let Some((path, meta)) = stack.pop() {
        if meta.dev() != device || (meta.is_dir() && mounts.contains(&path)) {
            continue;
        }
        if !meta.is_dir() && meta.nlink() > 1 {
            let entry = linked
                .entry(meta.ino())
                .or_insert((0, meta.nlink(), meta.blocks() * 512));
            entry.0 += 1;
            continue;
        }
        total += meta.blocks() * 512;
        if meta.is_dir() {
            for entry in fs::read_dir(&path).into_iter().flatten().flatten() {
                // `DirEntry::metadata` doesn't follow symlinks.
                if let Ok(meta) = entry.metadata() {
                    stack.push((entry.path(), meta));
                }
            }
        }
    }
    let freed_links: u64 = linked
        .values()
        .filter(|(seen, links, _)| seen >= links)
        .map(|(_, _, bytes)| bytes)
        .sum();
    total + freed_links
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testdir::TestDir;

    fn rules(home: &TestDir) -> Rules {
        Rules::new(home.path(), ALLOWED_ROOTS, &[]).unwrap()
    }

    fn paths(category: &Category) -> Vec<PathBuf> {
        category
            .items
            .iter()
            .map(|item| item.path().unwrap().to_path_buf())
            .collect()
    }

    #[test]
    fn claims_do_not_overlap() {
        let mut claims = Claims::default();
        assert!(claims.claim(Path::new("/h/.cache/pip")));
        assert!(!claims.claim(Path::new("/h/.cache/pip")));
        assert!(!claims.claim(Path::new("/h/.cache/pip/wheels")));
        assert!(!claims.claim(Path::new("/h/.cache")));
        assert!(claims.claim(Path::new("/h/.cache/pipx")));
    }

    #[test]
    fn specific_categories_claim_before_the_user_cache() {
        let home = TestDir::new();
        home.file(".cache/yay/pkg/a.tar", 5000);
        home.file(".cache/pip/http/b", 5000);
        home.file(".cache/go-build/c", 5000);
        home.file(".cache/thumbnails/d.png", 5000);
        home.file(".npm/_cacache/e", 5000);
        let found = home_categories(home.path(), &rules(&home), &Mounts::default());

        assert_eq!(paths(&found.yay), [home.path().join(".cache/yay")]);
        assert_eq!(
            paths(&found.developer),
            [
                home.path().join(".cache/pip"),
                home.path().join(".npm/_cacache"),
                home.path().join(".cache/go-build"),
            ]
        );
        assert_eq!(
            paths(&found.user_cache),
            [home.path().join(".cache/thumbnails")]
        );
    }

    #[test]
    fn user_cache_has_one_item_per_folder_largest_first() {
        let home = TestDir::new();
        home.file(".cache/small/a", 100);
        home.file(".cache/big/a", 100_000);
        home.file(".cache/loose-file", 100);
        home.symlink(".cache/linked", &home.dir("Documents"));
        let found = home_categories(home.path(), &rules(&home), &Mounts::default());
        assert_eq!(
            paths(&found.user_cache),
            [
                home.path().join(".cache/big"),
                home.path().join(".cache/small")
            ]
        );
    }

    #[test]
    fn nothing_is_pre_selected_and_caches_are_regenerable() {
        let home = TestDir::new();
        home.file(".cache/thumbnails/a", 100);
        home.file(".local/share/Trash/files/old.txt", 100);
        let found = home_categories(home.path(), &rules(&home), &Mounts::default());
        let cache = &found.user_cache.items[0];
        assert!(cache.is_regenerable() && !cache.selected);
        let trash = &found.trash.items[0];
        assert!(!trash.is_regenerable() && !trash.selected);
    }

    #[test]
    fn empty_trash_is_not_proposed() {
        let home = TestDir::new();
        home.dir(".local/share/Trash/files");
        home.dir(".local/share/Trash/info");
        let found = home_categories(home.path(), &rules(&home), &Mounts::default());
        assert!(found.trash.items.is_empty());
    }

    #[test]
    fn trash_is_not_read_through_symlinks() {
        let home = TestDir::new();
        let keys = home.file(".ssh/id_ed25519", 100_000);
        home.symlink(".local/share/Trash/files", keys.parent().unwrap());
        home.file(".local/share/Trash/info/old.trashinfo", 100);
        let rules = rules(&home);
        let found = home_categories(home.path(), &rules, &Mounts::default());
        assert!(found.trash.items[0].size < 100_000);
        let trash = home.path().join(TRASH);
        assert_eq!(trash_parts(&trash, &rules), [trash.join("info")]);
    }

    #[test]
    fn a_symlinked_trash_is_refused() {
        let home = TestDir::new();
        let documents = home.dir("Documents");
        home.file("Documents/files/thesis.pdf", 100_000);
        home.symlink(".local/share/Trash", &documents);
        let found = home_categories(home.path(), &rules(&home), &Mounts::default());
        assert!(found.trash.items.is_empty());
        assert!(found.trash.note.is_some());
    }

    #[test]
    fn protected_folders_are_left_out_and_noted() {
        let home = TestDir::new();
        home.file(".cache/keep/a", 100);
        home.file(".cache/other/a", 100);
        let rules = Rules::new(
            home.path(),
            ALLOWED_ROOTS,
            &[home.path().join(".cache/keep")],
        )
        .unwrap();
        let found = home_categories(home.path(), &rules, &Mounts::default());
        assert_eq!(paths(&found.user_cache), [home.path().join(".cache/other")]);
        assert!(found.user_cache.note.is_some());
    }

    #[test]
    fn reads_mount_points() {
        let mounts = Mounts::parse(
            "36 35 98:0 / /home/me/.cache/my\\040disk rw,noatime master:1 - ext3 /dev/root rw\n\
             37 35 98:0 /mnt /home/me/.cache/bind rw - ext4 /dev/sda1 rw\n",
        );
        assert!(mounts.contains(Path::new("/home/me/.cache/my disk")));
        assert!(mounts.contains(Path::new("/home/me/.cache/bind")));
        assert!(!mounts.contains(Path::new("/home/me/.cache")));
    }

    #[test]
    fn mount_points_are_not_walked_or_proposed() {
        let home = TestDir::new();
        home.file(".cache/app/a", 1000);
        let bind = home.dir(".cache/app/bind");
        home.file(".cache/app/bind/projects", 100_000);
        let mounted = home.dir(".cache/mounted");
        home.file(".cache/mounted/a", 100_000);
        let mounts = Mounts([bind, mounted].into_iter().collect());
        let found = home_categories(home.path(), &rules(&home), &mounts);
        assert_eq!(paths(&found.user_cache), [home.path().join(".cache/app")]);
        assert!(found.user_cache.items[0].size < 100_000);
        assert!(found.user_cache.note.is_some());
    }

    #[test]
    fn disk_usage_leaves_out_files_linked_from_elsewhere() {
        let home = TestDir::new();
        let file = home.file(".cache/uv/a", 100_000);
        home.dir("project");
        fs::hard_link(&file, home.path().join("project/a")).unwrap();
        assert!(disk_usage(&home.path().join(".cache/uv"), &Mounts::default()) < 100_000);
    }

    #[test]
    fn disk_usage_counts_blocks_and_hard_links_once() {
        let home = TestDir::new();
        let file = home.file("dir/a", 10_000);
        fs::hard_link(&file, home.path().join("dir/b")).unwrap();
        let one = fs::metadata(&file).unwrap().blocks() * 512;
        let dir = fs::metadata(home.path().join("dir")).unwrap().blocks() * 512;
        assert_eq!(
            disk_usage(&home.path().join("dir"), &Mounts::default()),
            dir + one
        );
    }

    #[test]
    fn disk_usage_does_not_follow_symlinks() {
        let home = TestDir::new();
        home.file("big/a", 100_000);
        let link = home.symlink("link", &home.path().join("big"));
        assert!(disk_usage(&link, &Mounts::default()) < 100_000);
    }
}
