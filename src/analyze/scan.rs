//! Walking the filesystem on several threads at once, without following symlinks or going into
//! other filesystems.

use std::collections::HashSet;
use std::fs::{self, Metadata};
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Condvar, Mutex};
use std::thread;

use super::tree::{Entry, Id, Kind, ROOT, Tree, Unwalked};
use crate::disk::Mounts;

/// How far a scan has got, for the screen to show while it runs.
#[derive(Debug, Default)]
pub struct Progress {
    /// Files and folders found so far.
    pub found: AtomicU64,
    /// The space they use.
    pub bytes: AtomicU64,
}

/// Folders read at once, at most. Reading mostly waits on the disk, which can take many requests
/// at a time: with 16, a home desktop's 1.8 million files take half a second once cached.
const THREADS: usize = 16;

const POISONED: &str = "a scanning thread panicked";

/// Walks everything under `root` into a [`Tree`]. The insides of mount points are left out, since
/// another filesystem is mounted there, and so are the folders vole may not read.
pub fn scan(root: &Path, mounts: &Mounts, progress: &Progress) -> Tree {
    let size = fs::symlink_metadata(root).map_or(0, |meta| meta.blocks() * 512);
    let walk = Walk {
        tree: Mutex::new(Tree::new(root.to_path_buf(), size)),
        queue: Mutex::new(Queue {
            waiting: vec![Folder {
                path: root.to_path_buf(),
                id: ROOT,
            }],
            reading: 0,
        }),
        wake: Condvar::new(),
        linked: Mutex::new(HashSet::new()),
        mounts,
        progress,
    };
    let threads = thread::available_parallelism().map_or(THREADS, |n| n.get().min(THREADS));
    thread::scope(|scope| {
        for _ in 0..threads {
            scope.spawn(|| walk.work());
        }
    });
    let mut tree = walk.tree.into_inner().expect(POISONED);
    tree.finish();
    tree
}

struct Walk<'a> {
    tree: Mutex<Tree>,
    queue: Mutex<Queue>,
    /// Woken when folders are queued, and when the last one has been read.
    wake: Condvar,
    /// The files with several hard links counted already, by device and inode.
    linked: Mutex<HashSet<(u64, u64)>>,
    mounts: &'a Mounts,
    progress: &'a Progress,
}

struct Queue {
    /// Folders waiting to be read.
    waiting: Vec<Folder>,
    /// How many are being read now.
    reading: usize,
}

/// A folder to read, and its place in the tree.
struct Folder {
    path: PathBuf,
    id: Id,
}

/// The folders found in one that was read, queued when dropped: even when reading it panicked,
/// so the other threads don't wait for it forever.
struct Read<'a, 'b> {
    walk: &'a Walk<'b>,
    found: Vec<Folder>,
}

impl Drop for Read<'_, '_> {
    fn drop(&mut self) {
        let Ok(mut queue) = self.walk.queue.lock() else {
            return;
        };
        queue.reading -= 1;
        let last = queue.reading == 0 && queue.waiting.is_empty() && self.found.is_empty();
        let found = !self.found.is_empty();
        queue.waiting.append(&mut self.found);
        drop(queue);
        if found || last {
            self.walk.wake.notify_all();
        }
    }
}

impl Walk<'_> {
    fn work(&self) {
        while let Some(folder) = self.next() {
            let mut read = Read {
                walk: self,
                found: Vec::new(),
            };
            read.found = self.read(&folder);
        }
    }

    /// The next folder to read; `None` once all are read.
    fn next(&self) -> Option<Folder> {
        let mut queue = self.queue.lock().expect(POISONED);
        loop {
            if let Some(folder) = queue.waiting.pop() {
                queue.reading += 1;
                return Some(folder);
            }
            if queue.reading == 0 {
                return None;
            }
            queue = self.wake.wait(queue).expect(POISONED);
        }
    }

    /// Reads one folder into the tree, and returns the folders in it, to read next.
    fn read(&self, folder: &Folder) -> Vec<Folder> {
        let Ok(listing) = fs::read_dir(&folder.path) else {
            self.tree.lock().expect(POISONED).mark_unreadable(folder.id);
            return Vec::new();
        };
        let mut entries = Vec::new();
        // The folders found, by their index in `entries`.
        let mut folders = Vec::new();
        let mut incomplete = false;
        let mut bytes = 0;
        for item in listing {
            let Ok(item) = item else {
                incomplete = true;
                continue;
            };
            let name = item.file_name();
            // The listing tells folders apart without looking at them, so a mount point isn't
            // even looked at: a network share that doesn't answer would keep the walk waiting.
            let path = item
                .file_type()
                .is_ok_and(|kind| kind.is_dir())
                .then(|| folder.path.join(&name));
            if path.as_ref().is_some_and(|path| self.mounts.contains(path)) {
                entries.push(Entry {
                    name,
                    size: 0,
                    kind: Kind::Dir,
                    unwalked: Some(Unwalked::Mount),
                    linked: false,
                });
                continue;
            }
            // Doesn't follow symlinks.
            let meta = match item.metadata() {
                Ok(meta) => meta,
                // Gone since the folder was listed.
                Err(err) if err.kind() == io::ErrorKind::NotFound => continue,
                Err(_) => {
                    incomplete = true;
                    continue;
                }
            };
            let kind = kind(&meta);
            let mut size = meta.blocks() * 512;
            let mut linked = false;
            if kind != Kind::Dir && meta.nlink() > 1 {
                // Counted at the first of its links the walk reaches.
                let mut counted = self.linked.lock().expect(POISONED);
                linked = !counted.insert((meta.dev(), meta.ino()));
                if linked {
                    size = 0;
                }
            }
            if kind == Kind::Dir {
                let path = path.unwrap_or_else(|| folder.path.join(&name));
                folders.push((entries.len(), path));
            }
            bytes += size;
            entries.push(Entry {
                name,
                size,
                kind,
                unwalked: None,
                linked,
            });
        }
        let found = entries.len() as u64;
        let mut tree = self.tree.lock().expect(POISONED);
        let first = tree.add(folder.id, entries);
        if incomplete {
            tree.mark_incomplete(folder.id);
        }
        drop(tree);
        self.progress.found.fetch_add(found, Ordering::Relaxed);
        self.progress.bytes.fetch_add(bytes, Ordering::Relaxed);
        folders
            .into_iter()
            .map(|(index, path)| Folder {
                path,
                id: first + index as Id,
            })
            .collect()
    }
}

fn kind(meta: &Metadata) -> Kind {
    let kind = meta.file_type();
    if kind.is_dir() {
        Kind::Dir
    } else if kind.is_file() {
        Kind::File
    } else if kind.is_symlink() {
        Kind::Symlink
    } else {
        Kind::Other
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::disk;
    use crate::testdir::TestDir;
    use std::fs::Permissions;
    use std::os::unix::fs::PermissionsExt;

    fn scan_dir(dir: &TestDir, mounts: &Mounts) -> Tree {
        scan(dir.path(), mounts, &Progress::default())
    }

    /// The id of `relative` in `tree`, found by name from the root.
    fn find(tree: &Tree, relative: &str) -> Id {
        let mut id = ROOT;
        for name in Path::new(relative) {
            id = tree
                .largest_first(id)
                .into_iter()
                .find(|&child| tree.name(child) == name)
                .unwrap_or_else(|| panic!("{relative} not in the tree"));
        }
        id
    }

    #[test]
    fn measures_like_disk_usage() {
        let dir = TestDir::new();
        dir.file("a/one", 10_000);
        dir.file("a/b/two", 100_000);
        dir.file("three", 1);
        let progress = Progress::default();
        let tree = scan(dir.path(), &Mounts::default(), &progress);
        let usage = |path: &Path| disk::disk_usage(path, &Mounts::default());
        assert_eq!(tree.size(ROOT), usage(dir.path()));
        assert_eq!(tree.size(find(&tree, "a")), usage(&dir.path().join("a")));
        assert_eq!(
            tree.size(find(&tree, "a/b/two")),
            usage(&dir.path().join("a/b/two"))
        );
        assert_eq!(tree.path(find(&tree, "a/b")), dir.path().join("a/b"));
        assert_eq!(tree.len(), 6);
        assert_eq!(progress.found.load(Ordering::Relaxed), 5);
    }

    #[test]
    fn does_not_follow_symlinks() {
        let dir = TestDir::new();
        let outside = TestDir::new();
        outside.file("big", 100_000);
        dir.symlink("link", outside.path());
        let tree = scan_dir(&dir, &Mounts::default());
        let link = find(&tree, "link");
        assert_eq!(tree.kind(link), Kind::Symlink);
        assert!(!tree.can_open(link));
        assert!(tree.size(ROOT) < 100_000);
    }

    #[test]
    fn leaves_mount_points_out() {
        let dir = TestDir::new();
        let mounted = dir.dir("mnt/disk");
        dir.file("mnt/disk/big", 100_000);
        let tree = scan_dir(&dir, &Mounts::from_paths([mounted]));
        let disk = find(&tree, "mnt/disk");
        assert_eq!(tree.unwalked(disk), Some(Unwalked::Mount));
        assert_eq!(tree.size(disk), 0);
        assert!(tree.largest_first(disk).is_empty());
    }

    #[test]
    fn counts_hard_linked_files_once() {
        let dir = TestDir::new();
        let file = dir.file("a/file", 100_000);
        dir.dir("b");
        fs::hard_link(&file, dir.path().join("b/file")).unwrap();
        let tree = scan_dir(&dir, &Mounts::default());
        let (a, b) = (find(&tree, "a/file"), find(&tree, "b/file"));
        assert_ne!(tree.linked(a), tree.linked(b));
        assert_eq!(
            tree.size(a) + tree.size(b),
            fs::metadata(&file).unwrap().blocks() * 512
        );
    }

    #[test]
    fn marks_what_it_may_not_read() {
        if rustix::process::geteuid().is_root() {
            return;
        }
        let dir = TestDir::new();
        let locked = dir.dir("deep/locked");
        dir.file("deep/locked/secret", 100);
        fs::set_permissions(&locked, Permissions::from_mode(0o000)).unwrap();
        let tree = scan_dir(&dir, &Mounts::default());
        fs::set_permissions(&locked, Permissions::from_mode(0o755)).unwrap();
        let id = find(&tree, "deep/locked");
        assert_eq!(tree.unwalked(id), Some(Unwalked::Unreadable));
        assert!(tree.incomplete(find(&tree, "deep")) && tree.incomplete(ROOT));
        assert!(!tree.can_open(id));
    }
}
