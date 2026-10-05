//! `orphans`: Orphaned Packages and Orphaned Configs.

mod configs;
mod detect;
mod known;
mod owners;
mod pacman;
pub mod plan;
mod remove;
mod screen;

use std::collections::HashMap;
use std::path::Path;
use std::thread;
use std::time::SystemTime;

use anyhow::{Context, Result};

use crate::config::Config;
use crate::disk::Mounts;
use crate::in_use;
use crate::safety::Rules;
use owners::Owners;
use plan::Plan;
pub use screen::Screen;

/// Builds the Plan. Only reads: nothing changes.
pub fn build_plan(home: &Path, config: &Config) -> Result<Plan> {
    let rules = Rules::new(home, configs::ROOTS, &config.protected_paths(home))
        .with_context(|| format!("resolving {}", home.display()))?
        .with_loose_dotfiles();
    // Measuring only: without the mount points, the walk still stops at other filesystems.
    let mounts = Mounts::load().unwrap_or_default();
    let (candidates, refused) = configs::candidates(home, &rules, &mounts);
    // These don't depend on each other.
    let (orphans, installed, files, open, outside) = thread::scope(|scope| {
        let orphans = scope.spawn(pacman::orphans);
        let installed = scope.spawn(pacman::installed);
        let open = scope.spawn(in_use::open_paths);
        let outside = scope.spawn(|| detect::OutsidePacman::read(home));
        let files = pacman::files();
        let join = "a measuring thread panicked";
        (
            orphans.join().expect(join),
            installed.join().expect(join),
            files,
            open.join().expect(join),
            outside.join().expect(join),
        )
    });
    let orphans = orphans.context("listing the orphaned packages")?;
    let installed = installed.context("reading the installed packages")?;
    let files = files.context("listing the installed files")?;

    let mut plan = Plan::build(&installed, &orphans);
    if !plan.packages.is_empty() {
        let names: Vec<&str> = plan.packages.iter().map(|p| p.name.as_str()).collect();
        let details = pacman::config_files(&names).context("reading the packages' config files")?;
        let mut config_files: HashMap<String, _> = details
            .into_iter()
            .map(|info| (info.name, info.config_files))
            .collect();
        for package in &mut plan.packages {
            package.config_files = config_files.remove(&package.name).unwrap_or_default();
        }
    }

    let names: Vec<String> = candidates.iter().map(configs::Candidate::name).collect();
    let wanted = owners::wanted(names.iter().map(String::as_str));
    let mut owners = Owners::from_pacman(&installed, &files, &wanted);
    owners.add_outside_pacman(&outside);
    if let Some(path) = known::path() {
        owners.add_known(known::load(&path, home)?);
    }
    plan.configs = configs::find(home, &candidates, &owners, &mounts, SystemTime::now());
    configs::mark_in_use(&mut plan.configs, &rules, &open);
    if refused > 0 {
        plan.configs_note = Some(format!("{refused} path(s) left out by the safety rules"));
    }
    Ok(plan)
}
