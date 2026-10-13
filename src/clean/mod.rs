//! `clean`: caches, pacman cache, journal and trash.

pub mod plan;
mod remove;
mod root;
mod scan;
mod screen;

use std::path::{Path, PathBuf};
use std::thread;

use anyhow::{Context, Result};

use crate::config::Config;
use crate::disk;
use crate::in_use;
use crate::safety::Rules;
use plan::{Item, Plan};
pub use root::{journal_usage, sees_whole_journal};
pub use screen::Screen;

/// Builds the Plan. Only reads: nothing on disk changes.
pub fn build_plan(home: &Path, config: &Config) -> Result<Plan> {
    let rules = Rules::new(home, scan::ALLOWED_ROOTS, &config.protected_paths(home))
        .with_context(|| format!("resolving {}", home.display()))?;
    // The measurements don't depend on each other, so the tools run while the disk is walked.
    let (pacman, journal, found, open) = thread::scope(|scope| {
        let pacman = scope.spawn(|| root::pacman(config.pacman_keep_versions));
        let journal = scope.spawn(|| root::journal(config.journal_max_mb));
        let open = scope.spawn(in_use::open_paths);
        // Measuring only: without the mount points, the walk still stops at other filesystems.
        let mounts = disk::Mounts::load().unwrap_or_default();
        let found = scan::home_categories(home, &rules, &mounts);
        let join = "a measuring thread panicked";
        (
            pacman.join().expect(join),
            journal.join().expect(join),
            found,
            open.join().expect(join),
        )
    });
    let mut plan = Plan {
        categories: vec![
            pacman,
            found.yay,
            found.developer,
            journal,
            found.user_cache,
            found.trash,
        ],
    };

    mark_in_use(plan.items_mut(), &rules, &open);
    Ok(plan)
}

/// Checks the selected Items against the programs running now, since one may have started
/// using a cache after the scan. Flags already set stay set.
pub fn refresh_in_use(plan: &mut Plan, config: &Config) {
    let Some(home) = dirs::home_dir() else {
        return;
    };
    let Ok(rules) = Rules::new(&home, scan::ALLOWED_ROOTS, &config.protected_paths(&home)) else {
        return;
    };
    let selected = plan.items_mut().filter(|item| item.selected);
    mark_in_use(selected, &rules, &in_use::open_paths());
}

fn mark_in_use<'a>(items: impl Iterator<Item = &'a mut Item>, rules: &Rules, open: &[PathBuf]) {
    for item in items {
        let in_use = item
            .path()
            .and_then(|path| rules.resolve(path))
            .is_some_and(|path| in_use::is_in_use(&path, open));
        item.in_use |= in_use;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testdir::TestDir;
    use plan::{Category, Kind};

    #[test]
    fn marks_items_held_now_and_keeps_earlier_flags() {
        let home = TestDir::new();
        let held = home.dir(".cache/browser");
        let earlier = home.dir(".cache/other");
        let rules = Rules::new(home.path(), scan::ALLOWED_ROOTS, &[]).unwrap();
        let mut category = Category::new("Test");
        category.items = vec![
            Item::new("held", Kind::Regenerable(held.clone()), 1),
            Item::new("earlier", Kind::Regenerable(earlier), 1),
        ];
        category.items[1].in_use = true;
        mark_in_use(
            category.items.iter_mut(),
            &rules,
            &[held.join("Cache/data_0")],
        );
        assert!(category.items.iter().all(|item| item.in_use));
    }
}
