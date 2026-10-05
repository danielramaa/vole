//! `orphans`: Orphaned Packages. Orphaned Configs come with M4.

mod pacman;
pub mod plan;
mod screen;

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
    Ok(Plan::build(&installed, &orphans))
}
