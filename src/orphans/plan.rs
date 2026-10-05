//! The Plan of `orphans`: Orphaned Packages, nothing selected until the user picks.
//!
//! Besides the packages `pacman -Qdtq` lists, the Plan has the ones that only become orphans
//! once those are removed. What is selected is always exactly what `pacman -Rns` removes when
//! given the selection: `-s` takes along the dependencies nothing else needs, so selecting a
//! package selects them too.

use std::collections::{HashMap, HashSet};

use super::pacman::{Backup, Info};

/// An Orphaned Package: one Item of the Plan.
#[derive(Debug, Clone)]
pub struct Package {
    pub name: String,
    /// Installed size in bytes.
    pub size: u64,
    /// Only an orphan once a package `pacman -Qdtq` lists is removed; shown under that package.
    pub nested: bool,
    /// The other packages `pacman -Qdtq` lists that must be removed too before this one goes.
    pub also_needs: Vec<String>,
    /// Installed packages that stay but list this one as an optional dependency: `pacman -Rns`
    /// removes it anyway once nothing depends on it.
    pub optional_for: Vec<String>,
    /// The config files that go with it: `-n` deletes even the changed ones.
    pub backup: Vec<Backup>,
    pub selected: bool,
    /// The packages of the Plan that depend on this one.
    required_by: Vec<usize>,
    /// What must be removed before this package is an orphan: the packages that depend on it,
    /// or, when none do, the ones that list it as an optional dependency.
    needs: Vec<usize>,
}

impl Package {
    pub fn new(name: impl Into<String>, size: u64) -> Self {
        Self {
            name: name.into(),
            size,
            nested: false,
            also_needs: Vec::new(),
            optional_for: Vec::new(),
            backup: Vec::new(),
            selected: false,
            required_by: Vec::new(),
            needs: Vec::new(),
        }
    }
}

/// The Orphaned Packages: each package `pacman -Qdtq` lists, by name, followed by the packages
/// that only become orphans once it is removed.
#[derive(Debug, Clone, Default)]
pub struct Plan {
    pub packages: Vec<Package>,
}

impl Plan {
    /// The Plan for the packages `pacman -Qdtq` listed (`orphans`), from what `installed`, every
    /// installed package, says about them.
    pub fn build(installed: &[Info], orphans: &[String]) -> Self {
        let system = System::new(installed);
        system.plan(&system.chain(orphans))
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

    /// Selects or deselects a package, together with the packages that go or stay with it.
    pub fn toggle(&mut self, index: usize) {
        if self.packages[index].selected {
            self.deselect(index);
        } else {
            self.select(index);
        }
    }

    /// Selects every package, or deselects them all when they already are.
    pub fn toggle_all(&mut self) {
        let select = !self.packages.iter().all(|package| package.selected);
        for package in &mut self.packages {
            package.selected = select;
        }
    }

    /// Selects a package, the packages it needs removed first, and what `pacman -Rns` takes
    /// along with them.
    fn select(&mut self, index: usize) {
        let mut selected = self.selection();
        selected[index] = true;
        loop {
            let before = selected.clone();
            for (p, package) in self.packages.iter().enumerate() {
                if before[p] {
                    for &q in &package.needs {
                        selected[q] = true;
                    }
                }
            }
            selected = self.removed_with(&selected);
            if selected == before {
                break;
            }
        }
        self.set_selection(&selected);
    }

    /// Deselects a package, the packages that can't go while it stays, and enough of the
    /// packages that depend on it that `pacman -Rns` doesn't take it along with them.
    fn deselect(&mut self, index: usize) {
        let mut selected = self.selection();
        let mut staying = vec![index];
        loop {
            while let Some(p) = staying.pop() {
                selected[p] = false;
                staying.extend(
                    (0..self.packages.len())
                        .filter(|&t| selected[t] && self.packages[t].needs.contains(&p)),
                );
            }
            let removed = self.removed_with(&selected);
            let taken: Vec<usize> = (0..removed.len())
                .filter(|&p| removed[p] && !selected[p])
                .collect();
            if taken.is_empty() {
                break;
            }
            for p in taken {
                let dependents = &self.packages[p].required_by;
                staying.extend(dependents.iter().copied().filter(|&t| selected[t]));
            }
        }
        self.set_selection(&selected);
    }

    fn selection(&self) -> Vec<bool> {
        self.packages
            .iter()
            .map(|package| package.selected)
            .collect()
    }

    fn set_selection(&mut self, selected: &[bool]) {
        for (package, &selected) in self.packages.iter_mut().zip(selected) {
            package.selected = selected;
        }
    }

    /// What `pacman -Rns` removes when given `targets`. Nothing outside the Plan depends on a
    /// package in it, so the Plan alone is enough to work it out.
    fn removed_with(&self, targets: &[bool]) -> Vec<bool> {
        let mut depends = vec![Vec::new(); self.packages.len()];
        for (p, package) in self.packages.iter().enumerate() {
            for &t in &package.required_by {
                depends[t].push(p);
            }
        }
        removed_with(&depends, &vec![true; depends.len()], targets)
    }
}

/// What `pacman -Rns` removes when given `targets`, the way libalpm's `_alpm_recursedeps` works
/// it out: the targets, and every package installed as a dependency (`removable`) that they
/// lead to through dependencies, except what a package that stays still depends on, directly or
/// through others. `depends[p]` are the packages `p` depends on.
fn removed_with(depends: &[Vec<usize>], removable: &[bool], targets: &[bool]) -> Vec<bool> {
    let n = depends.len();
    let mut candidate = vec![false; n];
    let mut stack: Vec<usize> = (0..n).filter(|&p| targets[p]).collect();
    while let Some(p) = stack.pop() {
        for &d in &depends[p] {
            if removable[d] && !targets[d] && !candidate[d] {
                candidate[d] = true;
                stack.push(d);
            }
        }
    }
    let mut stack: Vec<usize> = (0..n).filter(|&p| !targets[p] && !candidate[p]).collect();
    while let Some(p) = stack.pop() {
        for &d in &depends[p] {
            if candidate[d] {
                candidate[d] = false;
                stack.push(d);
            }
        }
    }
    (0..n).map(|p| targets[p] || candidate[p]).collect()
}

/// Every installed package, by index, and how they depend on each other.
struct System<'a> {
    infos: &'a [Info],
    index: HashMap<&'a str, usize>,
    required_by: Vec<Vec<usize>>,
    optional_for: Vec<Vec<usize>>,
    /// The packages each package depends on: Required By, the other way round.
    depends: Vec<Vec<usize>>,
    /// Installed as a dependency, and only depended on by packages pacman listed (an unknown
    /// name could be a package that stays).
    removable: Vec<bool>,
}

impl<'a> System<'a> {
    fn new(infos: &'a [Info]) -> Self {
        let index: HashMap<&str, usize> = infos
            .iter()
            .enumerate()
            .map(|(p, info)| (info.name.as_str(), p))
            .collect();
        let mut removable: Vec<bool> = infos.iter().map(|info| info.dependency).collect();
        let mut required_by = Vec::new();
        let mut optional_for = Vec::new();
        for (p, info) in infos.iter().enumerate() {
            let mut ids = |names: &[String]| -> Vec<usize> {
                let mut ids = Vec::new();
                for name in names {
                    match index.get(name.as_str()) {
                        Some(&id) if id != p => ids.push(id),
                        Some(_) => {}
                        None => removable[p] = false,
                    }
                }
                ids
            };
            required_by.push(ids(&info.required_by));
            optional_for.push(ids(&info.optional_for));
        }
        let mut depends = vec![Vec::new(); infos.len()];
        for (p, dependents) in required_by.iter().enumerate() {
            for &t in dependents {
                depends[t].push(p);
            }
        }
        Self {
            infos,
            index,
            required_by,
            optional_for,
            depends,
            removable,
        }
    }

    /// The packages that go when `orphans` are removed with `pacman -Rns`, again and again
    /// while that leaves new orphans: packages that were only optional dependencies of removed
    /// ones, which `-s` leaves alone and `pacman -Qdtq` lists next.
    fn chain(&self, orphans: &[String]) -> Vec<bool> {
        let n = self.infos.len();
        let mut chain = vec![false; n];
        for name in orphans {
            if let Some(&p) = self.index.get(name.as_str()) {
                chain[p] = true;
            }
        }
        loop {
            chain = removed_with(&self.depends, &self.removable, &chain);
            let next: Vec<usize> = (0..n)
                .filter(|&p| !chain[p] && self.is_orphan(p, &chain))
                .collect();
            if next.is_empty() {
                break;
            }
            for p in next {
                chain[p] = true;
            }
        }
        self.consistent(chain)
    }

    /// Whether `pacman -Qdtq` lists `p` once the packages in `gone` are removed.
    fn is_orphan(&self, p: usize, gone: &[bool]) -> bool {
        self.removable[p]
            && self.required_by[p]
                .iter()
                .chain(&self.optional_for[p])
                .all(|&q| gone[q])
    }

    /// `chain` without the packages that a package staying installed depends on (or lists as
    /// an optional dependency, when nothing depends on them), so what stays never needs what
    /// goes. Only `pacman -Qdtq` and `pacman -Qi` disagreeing, when packages change between
    /// the two, leaves any.
    fn consistent(&self, mut chain: Vec<bool>) -> Vec<bool> {
        loop {
            let stays: Vec<usize> = (0..chain.len())
                .filter(|&p| chain[p] && !self.can_go(p, &chain))
                .collect();
            if stays.is_empty() {
                return chain;
            }
            for p in stays {
                chain[p] = false;
            }
        }
    }

    fn can_go(&self, p: usize, chain: &[bool]) -> bool {
        self.removable[p] && self.needs(p).iter().all(|&q| chain[q])
    }

    /// What must be removed before `p` is an orphan (see [`Package`]).
    fn needs(&self, p: usize) -> &[usize] {
        if self.required_by[p].is_empty() {
            &self.optional_for[p]
        } else {
            &self.required_by[p]
        }
    }

    /// The packages of `chain` that need nothing else removed first, by name, each followed by
    /// the packages that wait for it. A package that waits for several comes after the first.
    fn plan(&self, chain: &[bool]) -> Plan {
        let name = |p: usize| self.infos[p].name.as_str();
        let mut members: Vec<usize> = (0..chain.len()).filter(|&p| chain[p]).collect();
        members.sort_by_key(|&p| name(p));
        // The packages each member waits for that need nothing themselves.
        let roots_of = |p: usize| -> Vec<usize> {
            let mut seen = HashSet::from([p]);
            let mut stack = vec![p];
            let mut roots = Vec::new();
            while let Some(q) = stack.pop() {
                if self.needs(q).is_empty() {
                    roots.push(q);
                }
                stack.extend(self.needs(q).iter().filter(|&&r| seen.insert(r)));
            }
            roots.sort_by_key(|&r| name(r));
            roots
        };
        let roots: HashMap<usize, Vec<usize>> = members.iter().map(|&p| (p, roots_of(p))).collect();
        // A package waiting only for itself, or for nothing (never left by the chain), leads.
        let leads = |p: usize| roots[&p].first().is_none_or(|&r| r == p);

        let mut order = Vec::new();
        for &lead in members.iter().filter(|&&p| leads(p)) {
            order.push(lead);
            order.extend(
                members
                    .iter()
                    .filter(|&&p| !leads(p) && roots[&p][0] == lead),
            );
        }
        let position: HashMap<usize, usize> =
            order.iter().enumerate().map(|(i, &p)| (p, i)).collect();
        let positions = |ids: &[usize]| ids.iter().map(|q| position[q]).collect();
        let packages = order
            .iter()
            .map(|&p| Package {
                nested: !leads(p),
                also_needs: if leads(p) {
                    Vec::new()
                } else {
                    roots[&p][1..]
                        .iter()
                        .map(|&r| name(r).to_string())
                        .collect()
                },
                optional_for: self.optional_for[p]
                    .iter()
                    .filter(|&&q| !chain[q])
                    .map(|&q| name(q).to_string())
                    .collect(),
                required_by: positions(&self.required_by[p]),
                needs: positions(self.needs(p)),
                ..Package::new(name(p), self.infos[p].size)
            })
            .collect();
        Plan { packages }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Installed packages, each with the packages it depends on, all installed as dependencies
    /// and of size 1. Required By is filled in from that, as pacman does.
    fn system(depends: &[(&str, &[&str])]) -> Vec<Info> {
        let mut infos: Vec<Info> = depends
            .iter()
            .map(|(name, _)| Info {
                name: name.to_string(),
                size: 1,
                dependency: true,
                ..Info::default()
            })
            .collect();
        for (name, dependencies) in depends {
            for dependency in *dependencies {
                info(&mut infos, dependency)
                    .required_by
                    .push(name.to_string());
            }
        }
        infos
    }

    fn info<'a>(infos: &'a mut [Info], name: &str) -> &'a mut Info {
        infos.iter_mut().find(|info| info.name == name).unwrap()
    }

    fn orphans(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| name.to_string()).collect()
    }

    /// The rows: nested packages indented, selected ones marked.
    fn rows(plan: &Plan) -> Vec<String> {
        plan.packages
            .iter()
            .map(|package| {
                let indent = if package.nested { "  " } else { "" };
                let mark = if package.selected { "*" } else { "" };
                format!("{indent}{}{mark}", package.name)
            })
            .collect()
    }

    fn index(plan: &Plan, name: &str) -> usize {
        plan.packages.iter().position(|p| p.name == name).unwrap()
    }

    fn toggle(plan: &mut Plan, name: &str) {
        let index = index(plan, name);
        plan.toggle(index);
    }

    #[test]
    fn groups_later_orphans_under_the_package_that_frees_them() {
        let installed = system(&[
            ("vala", &[]),
            ("meson", &["python-tqdm"]),
            ("python-tqdm", &["python-colorama"]),
            ("python-colorama", &[]),
        ]);
        let plan = Plan::build(&installed, &orphans(&["vala", "meson"]));
        assert_eq!(
            rows(&plan),
            ["meson", "  python-colorama", "  python-tqdm", "vala"]
        );
        assert_eq!(plan.selected_size(), 0);
    }

    #[test]
    fn leaves_what_a_package_that_stays_needs() {
        let mut installed = system(&[("a", &["shared"]), ("keeper", &["shared"]), ("shared", &[])]);
        info(&mut installed, "keeper").dependency = false;
        let plan = Plan::build(&installed, &orphans(&["a"]));
        assert_eq!(rows(&plan), ["a"]);
    }

    #[test]
    fn leaves_explicitly_installed_dependencies() {
        let mut installed = system(&[("a", &["tool"]), ("tool", &[])]);
        info(&mut installed, "tool").dependency = false;
        let plan = Plan::build(&installed, &orphans(&["a"]));
        assert_eq!(rows(&plan), ["a"]);
    }

    #[test]
    fn removes_dependency_cycles_together() {
        // Like lib32-krb5 and lib32-keyutils, which depend on each other.
        let installed = system(&[
            ("root", &["krb5"]),
            ("krb5", &["keyutils"]),
            ("keyutils", &["krb5"]),
        ]);
        let mut plan = Plan::build(&installed, &orphans(&["root"]));
        assert_eq!(rows(&plan), ["root", "  keyutils", "  krb5"]);
        toggle(&mut plan, "root");
        assert_eq!(plan.selected_size(), 3);
        toggle(&mut plan, "keyutils");
        assert_eq!(plan.selected_size(), 0);
    }

    #[test]
    fn selecting_a_package_selects_what_goes_with_it() {
        let installed = system(&[("a", &["b"]), ("b", &["c"]), ("c", &[]), ("other", &[])]);
        let mut plan = Plan::build(&installed, &orphans(&["a", "other"]));
        // pacman -Rns a removes b and c too.
        toggle(&mut plan, "a");
        assert_eq!(rows(&plan), ["a*", "  b*", "  c*", "other"]);
        // c can't stay while a goes: a depends on it through b.
        toggle(&mut plan, "c");
        assert_eq!(rows(&plan), ["a", "  b", "  c", "other"]);
        // And c can't go while a stays.
        toggle(&mut plan, "c");
        assert_eq!(rows(&plan), ["a*", "  b*", "  c*", "other"]);
    }

    #[test]
    fn a_dependency_of_two_orphans_goes_only_with_both() {
        let installed = system(&[("a", &["shared"]), ("b", &["shared"]), ("shared", &[])]);
        let mut plan = Plan::build(&installed, &orphans(&["a", "b"]));
        assert_eq!(rows(&plan), ["a", "  shared", "b"]);
        assert_eq!(plan.packages[1].also_needs, ["b"]);
        toggle(&mut plan, "a");
        assert_eq!(rows(&plan), ["a*", "  shared", "b"]);
        toggle(&mut plan, "b");
        assert_eq!(rows(&plan), ["a*", "  shared*", "b*"]);
        // Keeping a keeps shared, and b can still go.
        toggle(&mut plan, "a");
        assert_eq!(rows(&plan), ["a", "  shared", "b*"]);
    }

    #[test]
    fn optional_dependencies_become_orphans_in_the_next_round() {
        let mut installed = system(&[("a", &[]), ("plugin", &[])]);
        info(&mut installed, "plugin").optional_for = vec!["a".into()];
        let mut plan = Plan::build(&installed, &orphans(&["a"]));
        assert_eq!(rows(&plan), ["a", "  plugin"]);
        // pacman -Rns a leaves plugin, so it is picked on its own, and only with a.
        toggle(&mut plan, "a");
        assert_eq!(rows(&plan), ["a*", "  plugin"]);
        toggle(&mut plan, "a");
        toggle(&mut plan, "plugin");
        assert_eq!(rows(&plan), ["a*", "  plugin*"]);
        toggle(&mut plan, "a");
        assert_eq!(rows(&plan), ["a", "  plugin"]);
    }

    #[test]
    fn notes_dependencies_still_optional_for_packages_that_stay() {
        let mut installed = system(&[("a", &["lib"]), ("lib", &[]), ("keeper", &[])]);
        info(&mut installed, "keeper").dependency = false;
        info(&mut installed, "lib").optional_for = vec!["keeper".into()];
        let plan = Plan::build(&installed, &orphans(&["a"]));
        // pacman -Rns a removes lib anyway: keeper only lists it as optional.
        assert_eq!(rows(&plan), ["a", "  lib"]);
        assert_eq!(plan.packages[1].optional_for, ["keeper"]);
    }

    #[test]
    fn leaves_out_orphans_pacman_no_longer_agrees_on() {
        // -Qdtq listed a, but by the time of -Qi, keeper depends on it.
        let mut installed = system(&[("a", &["b"]), ("b", &[]), ("keeper", &["a"])]);
        info(&mut installed, "keeper").dependency = false;
        let plan = Plan::build(&installed, &orphans(&["a", "gone"]));
        assert!(plan.packages.is_empty());
    }

    #[test]
    fn toggle_all_selects_then_deselects() {
        let installed = system(&[("a", &["b"]), ("b", &[]), ("c", &[])]);
        let mut plan = Plan::build(&installed, &orphans(&["a", "c"]));
        toggle(&mut plan, "c");
        plan.toggle_all();
        assert_eq!(plan.selected_size(), 3);
        plan.toggle_all();
        assert_eq!(plan.selected_size(), 0);
    }
}
