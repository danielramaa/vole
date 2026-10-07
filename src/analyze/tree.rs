//! What the scan found: every file and folder under the folder it started from, with the space
//! each one uses.

use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use super::disks::Mount;

/// A file or folder of a [`Tree`].
pub type Id = u32;

/// The folder the scan started from.
pub const ROOT: Id = 0;

/// What an entry is, as `lstat` says: symlinks are never followed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    File,
    Dir,
    Symlink,
    /// A device, socket or named pipe.
    Other,
}

/// Why what is in a folder isn't in the tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unwalked {
    /// What is mounted on it isn't scanned (never [`Mount::SameDisk`]).
    Mount(Mount),
    /// The same folder as one reached by another path (through a bind mount), and counted
    /// there.
    Elsewhere,
    /// In a mount that isn't scanned, whose kind it says, but on the way to scanned mounts, as
    /// USB disks are in `/run/media`: only the folders on the way to them are in the tree.
    Through(Mount),
    /// vole may not read it.
    Unreadable,
}

/// An entry found in a folder, before it joins the tree.
pub struct Entry {
    pub name: OsString,
    /// Space used on disk: allocated blocks, so sparse files count what they really use.
    pub size: u64,
    pub kind: Kind,
    pub unwalked: Option<Unwalked>,
    /// A hard link to a file whose space is counted at another of its links.
    pub linked: bool,
}

/// Nodes are small, since a disk holds millions of files: names live in one buffer, and the
/// entries of a folder are next to each other, so a folder only says where they start.
#[derive(Debug, Clone)]
struct Node {
    /// The entry's own size, then, once the tree is finished, with everything inside it.
    size: u64,
    parent: Id,
    first_child: Id,
    children: u32,
    name_start: u32,
    /// A name is at most 255 bytes on Linux.
    name_len: u16,
    kind: Kind,
    unwalked: Option<Unwalked>,
    linked: bool,
    /// Something in it couldn't be read, so it uses at least `size`.
    incomplete: bool,
    /// Moved away since the scan: not listed any more.
    gone: bool,
}

pub struct Tree {
    root: PathBuf,
    nodes: Vec<Node>,
    names: Vec<u8>,
}

impl Tree {
    /// A tree holding only `root`, which uses `size` itself.
    pub fn new(root: PathBuf, size: u64) -> Self {
        let node = Node {
            size,
            parent: ROOT,
            first_child: 0,
            children: 0,
            name_start: 0,
            name_len: 0,
            kind: Kind::Dir,
            unwalked: None,
            linked: false,
            incomplete: false,
            gone: false,
        };
        Self {
            root,
            nodes: vec![node],
            names: Vec::new(),
        }
    }

    /// Adds what is in the folder `parent`, read all at once, and returns the first entry's id;
    /// the others follow it.
    pub fn add(&mut self, parent: Id, entries: Vec<Entry>) -> Id {
        let first = Self::id(self.nodes.len());
        for entry in entries {
            let bytes = entry.name.as_bytes();
            self.nodes.push(Node {
                size: entry.size,
                parent,
                first_child: 0,
                children: 0,
                name_start: Self::id(self.names.len()),
                name_len: u16::try_from(bytes.len()).expect("a name longer than Linux allows"),
                kind: entry.kind,
                unwalked: entry.unwalked,
                linked: entry.linked,
                incomplete: entry.unwalked == Some(Unwalked::Unreadable),
                gone: false,
            });
            self.names.extend_from_slice(bytes);
        }
        let children = Self::id(self.nodes.len()) - first;
        let node = &mut self.nodes[parent as usize];
        node.first_child = first;
        node.children = children;
        first
    }

    fn id(count: usize) -> u32 {
        u32::try_from(count).expect("more entries than a tree can hold")
    }

    /// Notes that the folder `id` couldn't be read.
    pub fn mark_unreadable(&mut self, id: Id) {
        let node = &mut self.nodes[id as usize];
        node.unwalked = Some(Unwalked::Unreadable);
        node.incomplete = true;
    }

    /// Notes that some of what is in the folder `id` couldn't be read.
    pub fn mark_incomplete(&mut self, id: Id) {
        self.nodes[id as usize].incomplete = true;
    }

    /// Adds each folder's insides to its size, once everything is in the tree.
    pub fn finish(&mut self) {
        // An entry is always added after its folder, so going backwards reaches every entry
        // before the folder it is in.
        for id in (1..self.nodes.len()).rev() {
            let Node {
                size,
                parent,
                incomplete,
                ..
            } = self.nodes[id];
            let parent = &mut self.nodes[parent as usize];
            parent.size += size;
            parent.incomplete |= incomplete;
        }
    }

    /// How many files and folders the tree holds, the root included.
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn name(&self, id: Id) -> &OsStr {
        let node = &self.nodes[id as usize];
        let start = node.name_start as usize;
        OsStr::from_bytes(&self.names[start..start + usize::from(node.name_len)])
    }

    pub fn path(&self, id: Id) -> PathBuf {
        let mut names = Vec::new();
        let mut id = id;
        while id != ROOT {
            names.push(self.name(id));
            id = self.nodes[id as usize].parent;
        }
        let mut path = self.root.clone();
        path.extend(names.iter().rev());
        path
    }

    pub fn size(&self, id: Id) -> u64 {
        self.nodes[id as usize].size
    }

    pub fn kind(&self, id: Id) -> Kind {
        self.nodes[id as usize].kind
    }

    pub fn unwalked(&self, id: Id) -> Option<Unwalked> {
        self.nodes[id as usize].unwalked
    }

    pub fn linked(&self, id: Id) -> bool {
        self.nodes[id as usize].linked
    }

    /// Whether something in `id` couldn't be read, so it uses at least its size.
    pub fn incomplete(&self, id: Id) -> bool {
        self.nodes[id as usize].incomplete
    }

    /// The folder `id` is in; `None` for the root.
    pub fn parent(&self, id: Id) -> Option<Id> {
        (id != ROOT).then(|| self.nodes[id as usize].parent)
    }

    /// The folders holding `id`, from the nearest up to the root.
    pub fn above(&self, id: Id) -> impl Iterator<Item = Id> + '_ {
        std::iter::successors(self.parent(id), |&id| self.parent(id))
    }

    /// Takes `id` out of the tree, once it has been moved away: it isn't listed any more, and the
    /// folders holding it lose its size.
    pub fn remove(&mut self, id: Id) {
        let node = &mut self.nodes[id as usize];
        if node.gone {
            return;
        }
        node.gone = true;
        let size = node.size;
        let above: Vec<Id> = self.above(id).collect();
        for folder in above {
            let folder = &mut self.nodes[folder as usize];
            folder.size = folder.size.saturating_sub(size);
        }
    }

    /// Adds `size` to the folder `id` and to those holding it, for what was moved into it.
    pub fn grow(&mut self, id: Id, size: u64) {
        let folders: Vec<Id> = std::iter::once(id).chain(self.above(id)).collect();
        for folder in folders {
            self.nodes[folder as usize].size += size;
        }
    }

    /// Whether `id`, or a folder holding it, has been taken out of the tree.
    pub fn is_gone(&self, id: Id) -> bool {
        std::iter::once(id)
            .chain(self.above(id))
            .any(|id| self.nodes[id as usize].gone)
    }

    /// Whether `id` is a folder whose insides are in the tree.
    pub fn can_open(&self, id: Id) -> bool {
        self.kind(id) == Kind::Dir && matches!(self.unwalked(id), None | Some(Unwalked::Through(_)))
    }

    /// What is in the folder `id`.
    fn children(&self, id: Id) -> impl Iterator<Item = Id> + '_ {
        let node = &self.nodes[id as usize];
        (node.first_child..node.first_child + node.children)
            .filter(|&child| !self.nodes[child as usize].gone)
    }

    /// The entry at `path`, when it is in the tree.
    pub fn find(&self, path: &Path) -> Option<Id> {
        let mut id = ROOT;
        for name in path.strip_prefix(&self.root).ok()? {
            id = self.children(id).find(|&child| self.name(child) == name)?;
        }
        Some(id)
    }

    /// What is in the folder `id`, largest first, then by name.
    pub fn largest_first(&self, id: Id) -> Vec<Id> {
        let mut children: Vec<Id> = self.children(id).collect();
        children.sort_by(|&a, &b| {
            self.size(b)
                .cmp(&self.size(a))
                .then_with(|| self.name(a).cmp(self.name(b)))
        });
        children
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, size: u64, kind: Kind) -> Entry {
        Entry {
            name: name.into(),
            size,
            kind,
            unwalked: None,
            linked: false,
        }
    }

    fn names(tree: &Tree, ids: Vec<Id>) -> Vec<&OsStr> {
        ids.into_iter().map(|id| tree.name(id)).collect()
    }

    /// /data holding `big` (a folder with two files) and `small`.
    fn tree() -> Tree {
        let mut tree = Tree::new("/data".into(), 4);
        let first = tree.add(
            ROOT,
            vec![entry("small", 10, Kind::File), entry("big", 4, Kind::Dir)],
        );
        tree.add(
            first + 1,
            vec![entry("a", 100, Kind::File), entry("b", 200, Kind::File)],
        );
        tree.finish();
        tree
    }

    #[test]
    fn folders_hold_the_size_of_what_is_inside() {
        let tree = tree();
        assert_eq!(tree.size(ROOT), 4 + 10 + 4 + 100 + 200);
        assert_eq!(tree.size(2), 304);
        assert_eq!(tree.len(), 5);
    }

    #[test]
    fn lists_folders_largest_first() {
        let tree = tree();
        assert_eq!(names(&tree, tree.largest_first(ROOT)), ["big", "small"]);
        let big = tree.largest_first(ROOT)[0];
        assert_eq!(tree.path(big), Path::new("/data/big"));
        let inside: Vec<PathBuf> = tree
            .largest_first(big)
            .into_iter()
            .map(|id| tree.path(id))
            .collect();
        assert_eq!(inside, [Path::new("/data/big/b"), Path::new("/data/big/a")]);
        assert_eq!(tree.parent(big), Some(ROOT));
        assert_eq!(tree.parent(ROOT), None);
        assert_eq!(tree.path(ROOT), Path::new("/data"));
        assert_eq!(tree.find(Path::new("/data/big/b")), Some(4));
        assert_eq!(tree.find(Path::new("/data")), Some(ROOT));
        assert_eq!(tree.find(Path::new("/data/big/c")), None);
        assert_eq!(tree.find(Path::new("/elsewhere")), None);
    }

    #[test]
    fn ties_go_by_name() {
        let mut tree = Tree::new("/".into(), 0);
        tree.add(
            ROOT,
            vec![entry("b", 1, Kind::File), entry("a", 1, Kind::File)],
        );
        assert_eq!(names(&tree, tree.largest_first(ROOT)), ["a", "b"]);
    }

    #[test]
    fn what_was_moved_away_leaves_the_tree() {
        let mut tree = tree();
        let big = tree.largest_first(ROOT)[0];
        let b = tree.largest_first(big)[0];
        assert_eq!(tree.above(b).collect::<Vec<_>>(), [big, ROOT]);
        tree.remove(b);
        tree.remove(b);
        assert_eq!(names(&tree, tree.largest_first(big)), ["a"]);
        assert_eq!(tree.size(big), 104);
        assert_eq!(tree.size(ROOT), 118);
        tree.remove(big);
        assert!(tree.is_gone(big) && tree.is_gone(big + 1) && !tree.is_gone(ROOT));
        assert_eq!(names(&tree, tree.largest_first(ROOT)), ["small"]);
        assert_eq!(tree.size(ROOT), 14);
    }

    #[test]
    fn what_could_not_be_read_marks_the_folders_above() {
        let mut tree = Tree::new("/".into(), 0);
        let mut locked = entry("locked", 4, Kind::Dir);
        locked.unwalked = Some(Unwalked::Unreadable);
        let first = tree.add(ROOT, vec![entry("open", 4, Kind::Dir), locked]);
        let deep = tree.add(first, vec![entry("deep", 4, Kind::Dir)]);
        tree.mark_incomplete(deep);
        tree.finish();
        assert!(tree.incomplete(ROOT) && tree.incomplete(first) && tree.incomplete(first + 1));
        assert!(!tree.can_open(first + 1) && tree.can_open(first));
    }
}
