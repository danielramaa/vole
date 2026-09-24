//! `clean`: caches, pacman cache, journal and trash.

mod in_use;
pub mod plan;
mod remove;
mod root;
mod scan;
mod screen;

use std::path::Path;
use std::thread;

use anyhow::{Context, Result};

use crate::config::Config;
use crate::safety::Rules;
use plan::Plan;
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
        let found = scan::home_categories(home, &rules, &scan::Mounts::load());
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

    for item in plan.items_mut() {
        let in_use = item
            .path()
            .and_then(|path| rules.resolve(path))
            .is_some_and(|path| in_use::is_in_use(&path, &open));
        item.in_use = in_use;
    }
    Ok(plan)
}
