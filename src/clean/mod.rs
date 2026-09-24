//! `clean`: caches, pacman cache, journal and trash.

mod in_use;
pub mod plan;
mod root;
mod scan;
mod screen;

use std::path::Path;

use anyhow::{Context, Result};

use crate::config::Config;
use crate::safety::Rules;
use plan::Plan;
pub use screen::Screen;

/// Builds the Plan. Only reads: nothing on disk changes.
pub fn build_plan(home: &Path, config: &Config) -> Result<Plan> {
    let rules = Rules::new(home, scan::ALLOWED_ROOTS, &config.protected_paths(home))
        .with_context(|| format!("resolving {}", home.display()))?;
    let found = scan::home_categories(home, &rules, &scan::Mounts::load());
    let mut plan = Plan {
        categories: vec![
            root::pacman(config.pacman_keep_versions),
            found.yay,
            found.developer,
            root::journal(config.journal_max_mb),
            found.user_cache,
            found.trash,
        ],
    };

    let open = in_use::open_paths();
    for item in plan.items_mut() {
        let in_use = item
            .path()
            .and_then(|path| rules.resolve(path))
            .is_some_and(|path| in_use::is_in_use(&path, &open));
        item.in_use = in_use;
    }
    Ok(plan)
}
