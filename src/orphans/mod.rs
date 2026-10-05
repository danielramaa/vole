//! `orphans`: Orphaned Packages. Orphaned Configs come with M4.

mod pacman;
pub mod plan;
mod remove;
mod screen;

use std::collections::HashMap;

use anyhow::{Context, Result};

use plan::Plan;
pub use screen::Screen;

/// Builds the Plan. Only asks pacman: nothing changes.
pub fn build_plan() -> Result<Plan> {
    let orphans = pacman::orphans().context("listing the orphaned packages")?;
    if orphans.is_empty() {
        return Ok(Plan::default());
    }
    let installed = pacman::installed().context("reading the installed packages")?;
    let mut plan = Plan::build(&installed, &orphans);
    if plan.packages.is_empty() {
        return Ok(plan);
    }
    let names: Vec<&str> = plan.packages.iter().map(|p| p.name.as_str()).collect();
    let details = pacman::backup_files(&names).context("reading the packages' config files")?;
    let mut backups: HashMap<String, _> = details
        .into_iter()
        .map(|info| (info.name, info.backup))
        .collect();
    for package in &mut plan.packages {
        package.backup = backups.remove(&package.name).unwrap_or_default();
    }
    Ok(plan)
}
