//! `orphans`: Orphaned Packages and Orphaned Configs.

mod backup;
mod configs;
mod detect;
mod known;
mod owners;
mod pacman;
pub mod plan;
mod remove;
mod restore;
mod screen;

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::SystemTime;

use anyhow::{Context, Result};

use crate::config::Config;
use crate::disk::Mounts;
use crate::in_use;
use crate::safety::Rules;
use owners::Owners;
use plan::Plan;
pub use restore::Screen as RestoreScreen;
pub use screen::Screen;

/// The path rules for Orphaned Configs: the roots, and the dotfiles directly in `home`.
fn rules(home: &Path, config: &Config) -> io::Result<Rules> {
    Ok(Rules::new(home, configs::ROOTS, &config.protected_paths(home))?.with_loose_dotfiles())
}

/// Builds the Plan. Only reads: nothing changes. The configs packages just removed `left`
/// behind come first.
pub fn build_plan(home: &Path, config: &Config, left: &[(PathBuf, String)]) -> Result<Plan> {
    let rules = rules(home, config).with_context(|| format!("resolving {}", home.display()))?;
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

    // Without every Owner, no config can be called orphaned; the packages can still be shown.
    let now = SystemTime::now();
    let owners = (|| -> Result<Owners> {
        let files = files.context("listing the installed files")?;
        let names: Vec<String> = candidates.iter().map(configs::Candidate::name).collect();
        let wanted = owners::wanted(names.iter().map(String::as_str));
        let mut owners = Owners::from_pacman(&installed, &files, &wanted);
        owners.add_outside_pacman(&outside);
        if let Some(path) = known::path() {
            owners.add_known(known::load(&path, home)?);
        }
        Ok(owners)
    })();
    match owners {
        Ok(owners) => {
            let found = configs::find(home, &candidates, &owners, &mounts, now);
            plan.configs = found.orphans;
            plan.package_owned = found.package_owned;
            configs::mark_left_behind(&mut plan.configs, left);
            configs::mark_in_use(&mut plan.configs, &rules, &open);
            if refused > 0 {
                plan.configs_note = Some(format!("{refused} path(s) left out by the safety rules"));
            }
        }
        // Shown on one row: some errors (TOML's) go on to quote the file.
        Err(err) => {
            let err = format!("{err:#}");
            plan.configs_error = Some(err.lines().next().unwrap_or_default().to_string());
        }
    }
    plan.backups = backup::old(home, &mounts, now);
    Ok(plan)
}

/// Checks the selected configs against the programs running now, since one may have started
/// using a config after the scan. Flags already set stay set.
pub fn refresh_in_use(plan: &mut Plan, config: &Config) {
    if plan.selected_configs().next().is_none() {
        return;
    }
    let Some(home) = dirs::home_dir() else {
        return;
    };
    let Ok(rules) = rules(&home, config) else {
        return;
    };
    let selected = plan.configs.iter_mut().filter(|config| config.selected);
    configs::mark_in_use(selected, &rules, &in_use::open_paths());
}
