//! The Plan of `clean`: Categories of Items, nothing selected until the user picks.

use std::path::{Path, PathBuf};

/// A root-owned cleanup, only ever done by an official tool through `sudo` (ADR 0001).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootCommand {
    /// Old versions of every package, installed or not, keeping this many of each
    /// (`paccache -k<n>`; paccache can't leave uninstalled packages out).
    PacmanKeep(u32),
    /// Every remaining version of uninstalled packages (`paccache -uk0`). Sized as what is left
    /// after `PacmanKeep`, so it runs after it and can't be selected without it.
    PacmanUninstalled,
    /// Shrink the systemd journal to this many MB (`journalctl --rotate --vacuum-size=<n>M`;
    /// rotating first lets the vacuum reach what is in the active files).
    JournalVacuum(u32),
}

pub const PACCACHE: &str = "/usr/bin/paccache";
pub const JOURNALCTL: &str = "/usr/bin/journalctl";

impl RootCommand {
    pub fn program(self) -> &'static str {
        match self {
            RootCommand::PacmanKeep(_) | RootCommand::PacmanUninstalled => PACCACHE,
            RootCommand::JournalVacuum(_) => JOURNALCTL,
        }
    }

    /// The arguments that apply the command: fixed flags and numbers only (ADR 0001).
    pub fn args(self) -> Vec<String> {
        match self {
            RootCommand::PacmanKeep(keep) => vec!["-r".into(), format!("-k{keep}")],
            RootCommand::PacmanUninstalled => vec!["-r".into(), "-u".into(), "-k0".into()],
            RootCommand::JournalVacuum(mb) => {
                vec!["--rotate".into(), format!("--vacuum-size={mb}M")]
            }
        }
    }

    /// Commands run in this order: `PacmanUninstalled` is sized as what `PacmanKeep` leaves.
    pub fn order(self) -> u8 {
        match self {
            RootCommand::PacmanKeep(_) => 0,
            RootCommand::PacmanUninstalled => 1,
            RootCommand::JournalVacuum(_) => 2,
        }
    }

    /// Whether `self` can only be selected together with `other`, because its size assumes
    /// `other` ran first.
    pub fn requires(self, other: RootCommand) -> bool {
        matches!(
            (self, other),
            (RootCommand::PacmanUninstalled, RootCommand::PacmanKeep(_))
        )
    }
}

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

    /// Selects or deselects one Item, together with the Items it requires (when selecting)
    /// or that require it (when deselecting), so what is removed always matches the sizes.
    pub fn toggle(&mut self, index: usize) {
        let select = !self.items[index].selected;
        self.items[index].selected = select;
        let Kind::Root(toggled) = self.items[index].kind else {
            return;
        };
        for item in &mut self.items {
            if let Kind::Root(other) = item.kind {
                if select && toggled.requires(other) {
                    item.selected = true;
                }
                if !select && other.requires(toggled) {
                    item.selected = false;
                }
            }
        }
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

    pub fn selected(&self) -> impl Iterator<Item = &Item> {
        self.categories
            .iter()
            .flat_map(|category| &category.items)
            .filter(|item| item.selected)
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
    fn root_commands_take_only_flags_and_numbers() {
        let commands = [
            RootCommand::PacmanKeep(2),
            RootCommand::PacmanUninstalled,
            RootCommand::JournalVacuum(200),
        ];
        for command in commands {
            assert!(command.program().starts_with("/usr/bin/"));
            for arg in command.args() {
                assert!(
                    arg.starts_with('-')
                        && arg
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || "-=".contains(c)),
                    "{arg}"
                );
            }
        }
        assert_eq!(RootCommand::PacmanKeep(3).args(), ["-r", "-k3"]);
        assert_eq!(
            RootCommand::JournalVacuum(200).args(),
            ["--rotate", "--vacuum-size=200M"]
        );
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
    fn uninstalled_packages_come_with_older_versions() {
        let mut category = Category::new("Pacman cache");
        category.items = vec![
            Item::new("older", Kind::Root(RootCommand::PacmanKeep(2)), 10),
            Item::new("uninstalled", Kind::Root(RootCommand::PacmanUninstalled), 5),
        ];
        category.toggle(1);
        assert_eq!(category.selected_size(), 15);
        category.toggle(0);
        assert_eq!(category.selected_size(), 0);
        // Older versions alone are fine.
        category.toggle(0);
        assert_eq!(category.selected_size(), 10);
        // Without the older-versions Item (nothing to trim), uninstalled stands alone.
        category.items.remove(0);
        category.items[0].selected = false;
        category.toggle(0);
        assert_eq!(category.selected_size(), 5);
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
