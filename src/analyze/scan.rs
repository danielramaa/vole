//! Walking the filesystem on several threads at once, without following symlinks or leaving the
//! disk it starts on.

use std::collections::HashSet;
use std::fs::{self, Metadata};
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Condvar, Mutex};
use std::thread;

use super::disks::{Mount, MountTable};
use super::tree::{Entry, Id, Kind, ROOT, Tree, Unwalked};

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

/// Walks everything under `root` into a [`Tree`], going into the mounts on the same disk, and on
/// `other_disks` too when asked. The folders vole may not read are left out.
pub fn scan(root: &Path, mounts: &MountTable, other_disks: bool, progress: &Progress) -> Tree {
    let meta = fs::symlink_metadata(root).ok();
    let walk = Walk {
        tree: Mutex::new(Tree::new(
            root.to_path_buf(),
            meta.as_ref().map_or(0, |meta| meta.blocks() * 512),
        )),
        queue: Mutex::new(Queue {
            waiting: vec![Folder {
                path: root.to_path_buf(),
                id: ROOT,
                through: false,
            }],
            reading: 0,
        }),
        wake: Condvar::new(),
        counted: Counted::default(),
        mounts,
        other_disks,
        scanned: mounts.scanned(other_disks),
        progress,
    };
    if let Some(meta) = &meta {
        walk.counted.first_time(meta);
    }
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
    counted: Counted,
    mounts: &'a MountTable,
    /// Whether the mounts of other disks are scanned too.
    other_disks: bool,
    /// The mount points scanned.
    scanned: Vec<PathBuf>,
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
    /// Only passed through, on the way to scanned mounts inside it.
    through: bool,
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

    /// Reads one folder into the tree, and returns the folders in it, to read next. Of a folder
    /// only passed through, only the folders on the way to scanned mounts are kept.
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
            // The listing tells folders apart without looking at them, so a mount point that
            // isn't scanned isn't even looked at: a network share that doesn't answer would keep
            // the walk waiting.
            let path = item
                .file_type()
                .is_ok_and(|kind| kind.is_dir())
                .then(|| folder.path.join(&name));
            let mount = path.as_ref().and_then(|path| self.mounts.get(path));
            let walked = match mount {
                Some(mount) => mount.is_scanned(self.other_disks),
                None => !folder.through,
            };
            if !walked {
                // Passed through when scanned mounts are inside, but never a network share.
                let through = mount != Some(Mount::Network)
                    && path
                        .as_ref()
                        .is_some_and(|path| self.leads_to_scanned(path));
                let unwalked = match (path, mount) {
                    (Some(path), _) if through => {
                        folders.push((entries.len(), path, true));
                        Unwalked::Through
                    }
                    (_, Some(mount)) if !folder.through => Unwalked::Mount(mount),
                    // Passing through, only the way to scanned mounts is kept.
                    _ => continue,
                };
                entries.push(Entry {
                    name,
                    size: 0,
                    kind: Kind::Dir,
                    unwalked: Some(unwalked),
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
            let mut unwalked = None;
            let mut linked = false;
            // Counted where the walk reaches it first.
            if kind == Kind::Dir && !self.counted.first_time(&meta) {
                unwalked = Some(Unwalked::Elsewhere);
                size = 0;
            } else if kind != Kind::Dir && meta.nlink() > 1 && !self.counted.first_time(&meta) {
                linked = true;
                size = 0;
            }
            if kind == Kind::Dir && unwalked.is_none() {
                let path = path.unwrap_or_else(|| folder.path.join(&name));
                folders.push((entries.len(), path, false));
            }
            bytes += size;
            entries.push(Entry {
                name,
                size,
                kind,
                unwalked,
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
            .map(|(index, path, through)| Folder {
                path,
                id: first + index as Id,
                through,
            })
            .collect()
    }

    /// Whether a scanned mount is inside `path`.
    fn leads_to_scanned(&self, path: &Path) -> bool {
        self.scanned
            .iter()
            .any(|mount| mount != path && mount.starts_with(path))
    }
}

/// The folders, and the files with several hard links, the walk has counted: by device and inode,
/// since a bind mount shows a folder at a second path, and a hard link shows a file.
#[derive(Default)]
struct Counted(Mutex<HashSet<(u64, u64)>>);

impl Counted {
    /// Whether this is the first time the walk reaches what `meta` is about.
    fn first_time(&self, meta: &Metadata) -> bool {
        let mut counted = self.0.lock().expect(POISONED);
        counted.insert((meta.dev(), meta.ino()))
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
    use crate::disk::{self, Mounts};
    use crate::testdir::TestDir;
    use std::fs::Permissions;
    use std::os::unix::fs::PermissionsExt;

    fn scan_dir(dir: &TestDir, mounts: &MountTable) -> Tree {
        scan(dir.path(), mounts, false, &Progress::default())
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
        let tree = scan(dir.path(), &MountTable::default(), false, &progress);
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
        let tree = scan_dir(&dir, &MountTable::default());
        let link = find(&tree, "link");
        assert_eq!(tree.kind(link), Kind::Symlink);
        assert!(!tree.can_open(link));
        assert!(tree.size(ROOT) < 100_000);
    }

    #[test]
    fn goes_into_mounts_on_the_same_disk_only() {
        let dir = TestDir::new();
        let boot = dir.dir("boot");
        dir.file("boot/vmlinuz", 1000);
        let mut mounts = vec![(boot, Mount::SameDisk)];
        for (name, mount) in [
            ("other", Mount::OtherDisk),
            ("usb", Mount::UsbDisk),
            ("nas", Mount::Network),
            ("efi", Mount::Automount),
            ("proc", Mount::NotADisk),
        ] {
            mounts.push((dir.dir(&format!("mnt/{name}")), mount));
            dir.file(&format!("mnt/{name}/big"), 100_000);
        }
        let tree = scan_dir(&dir, &MountTable::from_mounts(mounts));
        assert!(tree.size(find(&tree, "boot/vmlinuz")) > 0);
        let usb = find(&tree, "mnt/usb");
        assert_eq!(tree.unwalked(usb), Some(Unwalked::Mount(Mount::UsbDisk)));
        assert_eq!(tree.size(usb), 0);
        assert!(tree.largest_first(usb).is_empty());
        assert!(tree.size(ROOT) < 100_000);
    }

    #[test]
    fn goes_into_other_disks_when_asked_even_through_run() {
        let dir = TestDir::new();
        let run = dir.dir("run");
        dir.file("run/lock", 100);
        dir.file("run/user/1000/bus", 100);
        let usb = dir.dir("run/media/me/STICK");
        dir.file("run/media/me/STICK/photo.jpg", 50_000);
        let data = dir.dir("mnt/data");
        dir.file("mnt/data/big", 100_000);
        let nas = dir.dir("mnt/nas");
        let mounts = MountTable::from_mounts([
            (run, Mount::NotADisk),
            (usb, Mount::UsbDisk),
            (data, Mount::OtherDisk),
            (nas, Mount::Network),
        ]);
        let tree = scan(dir.path(), &mounts, true, &Progress::default());
        assert!(tree.size(find(&tree, "mnt/data/big")) > 0);
        assert!(tree.size(find(&tree, "run/media/me/STICK/photo.jpg")) > 0);
        // /run is only passed through: none of its own files are in the tree.
        let run = find(&tree, "run");
        assert_eq!(tree.unwalked(run), Some(Unwalked::Through));
        assert!(tree.can_open(run));
        let names: Vec<_> = tree
            .largest_first(run)
            .into_iter()
            .map(|id| tree.name(id))
            .collect();
        assert_eq!(names, ["media"]);
        assert_eq!(tree.size(run), tree.size(find(&tree, "run/media/me/STICK")));
        assert_eq!(
            tree.unwalked(find(&tree, "mnt/nas")),
            Some(Unwalked::Mount(Mount::Network))
        );
        // Without other disks, /run is left out whole.
        let tree = scan(dir.path(), &mounts, false, &Progress::default());
        assert_eq!(
            tree.unwalked(find(&tree, "run")),
            Some(Unwalked::Mount(Mount::NotADisk))
        );
        assert_eq!(
            tree.unwalked(find(&tree, "mnt/data")),
            Some(Unwalked::Mount(Mount::OtherDisk))
        );
    }

    #[test]
    fn counts_a_folder_reached_twice_once() {
        let dir = TestDir::new();
        let meta = fs::symlink_metadata(dir.dir("a")).unwrap();
        // As through a bind mount, which can't be made without root.
        let counted = Counted::default();
        assert!(counted.first_time(&meta));
        assert!(!counted.first_time(&meta));
        let tree = scan_dir(&dir, &MountTable::default());
        assert_eq!(tree.unwalked(find(&tree, "a")), None);
    }

    #[test]
    fn counts_hard_linked_files_once() {
        let dir = TestDir::new();
        let file = dir.file("a/file", 100_000);
        dir.dir("b");
        fs::hard_link(&file, dir.path().join("b/file")).unwrap();
        let tree = scan_dir(&dir, &MountTable::default());
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
        let tree = scan_dir(&dir, &MountTable::default());
        fs::set_permissions(&locked, Permissions::from_mode(0o755)).unwrap();
        let id = find(&tree, "deep/locked");
        assert_eq!(tree.unwalked(id), Some(Unwalked::Unreadable));
        assert!(tree.incomplete(find(&tree, "deep")) && tree.incomplete(ROOT));
        assert!(!tree.can_open(id));
    }
}
