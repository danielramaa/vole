//! The Plan of `clean`: Categories of Items, nothing selected until the user picks.

use std::path::{Path, PathBuf};

/// A root-owned cleanup, only ever done by an official tool through `sudo` (ADR 0001).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootCommand {
    /// Old versions of installed packages, keeping this many of each (`paccache -k<n>`).
    /// paccache applies this to uninstalled packages too, so `PacmanUninstalled` runs first.
    PacmanKeep(u32),
    /// Every cached version of uninstalled packages (`paccache -uk0`).
    PacmanUninstalled,
    /// Shrink the systemd journal to this many MB (`journalctl --rotate --vacuum-size=<n>M`;
    /// rotating first lets the vacuum reach what is in the active files).
    JournalVacuum(u32),
}

pub const PACCACHE: &str = "/usr/bin/paccache";
pub const JOURNALCTL: &str = "/usr/bin/journalctl";

/// What an Item is, and so how it may be removed. Every kind has exactly one way (ADR 0003).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    /// Recreated on demand, like a cache. Deleted permanently.
    Regenerable(PathBuf),
    /// The user's trash folder; emptying it is its own Item.
    Trash(PathBuf),
    /// Root-owned; measured and removed only through an official tool.
    Root(RootCommand),
}

#[derive(Debug, Clone)]
pub struct Item {
    pub label: String,
    pub kind: Kind,
    /// Bytes freed by removing the Item.
    pub size: u64,
    /// Held by a running process.
    pub in_use: bool,
    pub selected: bool,
}

impl Item {
    pub fn new(label: impl Into<String>, kind: Kind, size: u64) -> Self {
        Self {
            label: label.into(),
            kind,
            size,
            in_use: false,
            selected: false,
        }
    }

    pub fn path(&self) -> Option<&Path> {
        match &self.kind {
            Kind::Regenerable(path) | Kind::Trash(path) => Some(path),
            Kind::Root(_) => None,
        }
    }

    pub fn is_regenerable(&self) -> bool {
        matches!(self.kind, Kind::Regenerable(_))
    }
}

#[derive(Debug, Clone)]
pub struct Category {
    pub name: &'static str,
    pub items: Vec<Item>,
    /// Shown next to the Category, e.g. why it could not be measured.
    pub note: Option<String>,
}

impl Category {
    pub fn new(name: &'static str) -> Self {
        Self {
            name,
            items: Vec::new(),
            note: None,
        }
    }

    pub fn size(&self) -> u64 {
        self.items.iter().map(|item| item.size).sum()
    }

    pub fn selected_size(&self) -> u64 {
        self.items
            .iter()
            .filter(|item| item.selected)
            .map(|item| item.size)
            .sum()
    }

    /// Selects every Item, or deselects them all when they already are.
    pub fn toggle_all(&mut self) {
        let select = !self.items.iter().all(|item| item.selected);
        for item in &mut self.items {
            item.selected = select;
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Plan {
    pub categories: Vec<Category>,
}

impl Plan {
    pub fn size(&self) -> u64 {
        self.categories.iter().map(Category::size).sum()
    }

    pub fn selected_size(&self) -> u64 {
        self.categories.iter().map(Category::selected_size).sum()
    }

    pub fn items_mut(&mut self) -> impl Iterator<Item = &mut Item> {
        self.categories
            .iter_mut()
            .flat_map(|category| category.items.iter_mut())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn category() -> Category {
        let mut category = Category::new("Test");
        category.items = vec![
            Item::new("a", Kind::Regenerable("/a".into()), 10),
            Item::new("b", Kind::Trash("/b".into()), 5),
        ];
        category
    }

    #[test]
    fn nothing_starts_selected() {
        let plan = Plan {
            categories: vec![category()],
        };
        assert_eq!(plan.size(), 15);
        assert_eq!(plan.selected_size(), 0);
    }

    #[test]
    fn toggle_all_selects_then_deselects() {
        let mut category = category();
        category.items[0].selected = true;
        category.toggle_all();
        assert_eq!(category.selected_size(), 15);
        category.toggle_all();
        assert_eq!(category.selected_size(), 0);
    }
}
