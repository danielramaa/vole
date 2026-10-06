//! The disk as vole measures and changes it: mount points, space used, and removing folders
//! without leaving them.

use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::fs::{self, Metadata, Permissions};
use std::io;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Whether `path` is a folder itself, not a symlink to one.
pub fn is_real_dir(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|meta| meta.is_dir())
}

/// Every mount point, bind mounts included, from `/proc/self/mountinfo`.
#[derive(Default)]
pub struct Mounts(HashSet<PathBuf>);

impl Mounts {
    pub fn load() -> io::Result<Self> {
        fs::read_to_string("/proc/self/mountinfo").map(|text| Self::parse(&text))
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

    #[cfg(test)]
    pub fn from_paths(paths: impl IntoIterator<Item = PathBuf>) -> Self {
        Self(paths.into_iter().collect())
    }
}

/// Decodes the `\040`-style escapes mountinfo uses for spaces and other special bytes.
pub fn unescape_octal(field: &str) -> PathBuf {
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
/// other filesystems or mount points. A hard-linked file only counts when all of its links are
/// inside `path`, since otherwise removing `path` doesn't free it.
pub fn disk_usage(path: &Path, mounts: &Mounts) -> u64 {
    usage(path, mounts).bytes
}

/// [`disk_usage`] of several paths removed together, measured in one walk.
pub fn disk_usage_all(paths: &[PathBuf], mounts: &Mounts) -> u64 {
    usage_all(paths, mounts).bytes
}

/// The space under some paths, and when anything there last changed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Usage {
    /// Bytes freed by removing the paths (see [`disk_usage`]).
    pub bytes: u64,
    /// The newest modification time of what was walked; `None` when nothing could be read.
    pub newest: Option<SystemTime>,
}

/// The [`Usage`] of `path`.
pub fn usage(path: &Path, mounts: &Mounts) -> Usage {
    usage_all(std::slice::from_ref(&path.to_path_buf()), mounts)
}

/// The [`Usage`] of several paths removed together, measured in one walk.
pub fn usage_all(paths: &[PathBuf], mounts: &Mounts) -> Usage {
    // (path, metadata, device of the path it was reached from)
    let mut stack: Vec<(PathBuf, Metadata, u64)> = Vec::new();
    for path in paths {
        let Ok(meta) = fs::symlink_metadata(path) else {
            continue;
        };
        // Walk the real path, so it can be compared with the mount points.
        let path = if meta.is_symlink() {
            path.clone()
        } else {
            fs::canonicalize(path).unwrap_or_else(|_| path.clone())
        };
        let device = meta.dev();
        stack.push((path, meta, device));
    }
    // (device, inode) → (links seen, links in total, bytes).
    let mut linked: HashMap<(u64, u64), (u64, u64, u64)> = HashMap::new();
    let mut total = 0;
    let mut newest = None;
    while let Some((path, meta, device)) = stack.pop() {
        if meta.dev() != device || (meta.is_dir() && mounts.contains(&path)) {
            continue;
        }
        newest = newest.max(meta.modified().ok());
        if !meta.is_dir() && meta.nlink() > 1 {
            let entry = linked.entry((meta.dev(), meta.ino())).or_insert((
                0,
                meta.nlink(),
                meta.blocks() * 512,
            ));
            entry.0 += 1;
            continue;
        }
        total += meta.blocks() * 512;
        if meta.is_dir() {
            for entry in fs::read_dir(&path).into_iter().flatten().flatten() {
                // `DirEntry::metadata` doesn't follow symlinks.
                if let Ok(meta) = entry.metadata() {
                    stack.push((entry.path(), meta, device));
                }
            }
        }
    }
    let freed_links: u64 = linked
        .values()
        .filter(|(seen, links, _)| seen >= links)
        .map(|(_, _, bytes)| bytes)
        .sum();
    Usage {
        bytes: total + freed_links,
        newest,
    }
}

/// Moves `from` to `to` unless something is at `to` already, checked and moved in one step
/// (`RENAME_NOREPLACE`), so nothing made meanwhile is overwritten. On a filesystem without that,
/// it checks first, then moves.
pub fn rename_no_overwrite(from: &Path, to: &Path) -> io::Result<()> {
    use rustix::fs::{CWD, RenameFlags, renameat_with};
    match renameat_with(CWD, from, CWD, to, RenameFlags::NOREPLACE) {
        Err(err) if err == rustix::io::Errno::INVAL || err == rustix::io::Errno::NOSYS => {}
        result => return result.map_err(io::Error::from),
    }
    match fs::symlink_metadata(to) {
        Ok(_) => Err(io::Error::from(io::ErrorKind::AlreadyExists)),
        Err(err) if err.kind() == io::ErrorKind::NotFound => fs::rename(from, to),
        Err(err) => Err(err),
    }
}

/// Removes `path` (a real path) without following symlinks and without entering other
/// filesystems or mount points. Read-only folders, like Go's module cache, are made writable.
/// Anything that disappears meanwhile (a program cleaning its own cache) counts as removed.
pub fn remove_all(path: &Path, mounts: &Mounts) -> io::Result<()> {
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(err) => return gone_is_fine(Err(err)),
    };
    // The folder above is the baseline, so an Item that is itself on another filesystem is
    // caught even without the mount points.
    let parent = path.parent().unwrap_or(path);
    let device = fs::symlink_metadata(parent)?.dev();
    remove_tree(path, &meta, device, mounts)
}

fn gone_is_fine(result: io::Result<()>) -> io::Result<()> {
    match result {
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

fn remove_tree(path: &Path, meta: &fs::Metadata, device: u64, mounts: &Mounts) -> io::Result<()> {
    if !meta.is_dir() {
        return gone_is_fine(fs::remove_file(path));
    }
    if meta.dev() != device || mounts.contains(path) {
        return Err(io::Error::other(format!(
            "{} is a mount point",
            path.display()
        )));
    }
    let mode = meta.permissions().mode();
    if mode & 0o700 != 0o700 {
        gone_is_fine(fs::set_permissions(
            path,
            Permissions::from_mode(mode | 0o700),
        ))?;
    }
    let entries = match fs::read_dir(path) {
        Ok(entries) => entries,
        Err(err) => return gone_is_fine(Err(err)),
    };
    let mut first_error = None;
    for entry in entries {
        let result = entry.and_then(|entry| match entry.metadata() {
            Ok(meta) => remove_tree(&entry.path(), &meta, device, mounts),
            Err(err) => gone_is_fine(Err(err)),
        });
        if let Err(err) = result {
            first_error.get_or_insert(err);
        }
    }
    match first_error {
        Some(err) => Err(err),
        None => gone_is_fine(fs::remove_dir(path)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testdir::TestDir;
    use std::time::Duration;

    #[test]
    fn moves_without_overwriting() {
        let dir = TestDir::new();
        let from = dir.file("from", 1);
        let taken = dir.file("taken", 2);
        let err = rename_no_overwrite(&from, &taken).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&taken).unwrap().len(), 2);
        // Not even an empty folder is replaced.
        let folder = dir.dir("folder");
        let empty = dir.dir("empty");
        assert!(rename_no_overwrite(&folder, &empty).is_err());
        let free = dir.path().join("free");
        rename_no_overwrite(&from, &free).unwrap();
        assert!(free.exists() && !from.exists());
    }

    #[test]
    fn usage_says_when_anything_inside_last_changed() {
        let home = TestDir::new();
        let old = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000);
        let new = old + Duration::from_secs(86_400);
        let set = |path: &Path, time| fs::File::open(path).unwrap().set_modified(time).unwrap();
        let file = home.file("app/deep/data", 10);
        set(&file, new);
        for dir in ["app/deep", "app"] {
            set(&home.path().join(dir), old);
        }
        assert_eq!(
            usage(&home.path().join("app"), &Mounts::default()).newest,
            Some(new)
        );
        assert_eq!(
            usage(&home.path().join("missing"), &Mounts::default()).newest,
            None
        );
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
    fn loads_the_mount_points_of_this_system() {
        assert!(Mounts::load().unwrap().contains(Path::new("/")));
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
