//! The Plan of `orphans`: Orphaned Packages, nothing selected until the user picks.

use std::collections::HashSet;

use super::pacman::Info;

/// An Orphaned Package: one Item of the Plan.
#[derive(Debug, Clone)]
pub struct Package {
    pub name: String,
    /// Installed size in bytes.
    pub size: u64,
    pub selected: bool,
}

/// The Orphaned Packages, sorted by name.
#[derive(Debug, Clone, Default)]
pub struct Plan {
    pub packages: Vec<Package>,
}

impl Plan {
    /// The Plan for the packages `pacman -Qdtq` listed (`orphans`), with what `installed` says
    /// about them.
    pub fn build(installed: &[Info], orphans: &[String]) -> Self {
        let orphans: HashSet<&str> = orphans.iter().map(String::as_str).collect();
        let mut packages: Vec<Package> = installed
            .iter()
            .filter(|info| orphans.contains(info.name.as_str()))
            .map(|info| Package {
                name: info.name.clone(),
                size: info.size,
                selected: false,
            })
            .collect();
        packages.sort_by(|a, b| a.name.cmp(&b.name));
        Self { packages }
    }

    pub fn size(&self) -> u64 {
        self.packages.iter().map(|package| package.size).sum()
    }

    pub fn selected_size(&self) -> u64 {
        self.selected().map(|package| package.size).sum()
    }

    pub fn selected(&self) -> impl Iterator<Item = &Package> {
        self.packages.iter().filter(|package| package.selected)
    }

    pub fn toggle(&mut self, index: usize) {
        self.packages[index].selected = !self.packages[index].selected;
    }

    /// Selects every package, or deselects them all when they already are.
    pub fn toggle_all(&mut self) {
        let select = !self.packages.iter().all(|package| package.selected);
        for package in &mut self.packages {
            package.selected = select;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(name: &str, size: u64) -> Info {
        Info {
            name: name.into(),
            size,
        }
    }

    #[test]
    fn lists_the_orphans_by_name_with_nothing_selected() {
        let installed = [info("zstd", 1), info("meson", 10), info("asar", 5)];
        let plan = Plan::build(&installed, &["meson".into(), "asar".into(), "gone".into()]);
        let names: Vec<&str> = plan.packages.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["asar", "meson"]);
        assert_eq!(plan.size(), 15);
        assert_eq!(plan.selected_size(), 0);
    }

    #[test]
    fn toggle_all_selects_then_deselects() {
        let installed = [info("a", 1), info("b", 2)];
        let mut plan = Plan::build(&installed, &["a".into(), "b".into()]);
        plan.toggle(0);
        plan.toggle_all();
        assert_eq!(plan.selected_size(), 3);
        plan.toggle_all();
        assert_eq!(plan.selected_size(), 0);
    }
}
