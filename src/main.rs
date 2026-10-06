mod app;
mod clean;
mod config;
mod date;
mod disk;
mod in_use;
mod log;
mod orphans;
mod pacman;
mod safety;
mod size;
mod term;
#[cfg(test)]
mod testdir;
mod ui;

use anyhow::{Result, bail};
use clap::Parser;

use crate::app::{App, Module};
use crate::config::Config;

/// Clean, analyze and maintain an Arch Linux system.
#[derive(Parser)]
#[command(version, about)]
struct Cli {
    /// Module to open directly (default: the menu)
    #[arg(value_enum)]
    module: Option<Module>,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    if rustix::process::geteuid().is_root() {
        bail!(
            "vole must not be run as root; run it as your normal user (it asks for sudo when needed)"
        );
    }

    let config = Config::load()?;

    let mut terminal = ratatui::init();
    let result = App::new(cli.module, config).run(&mut terminal);
    ratatui::restore();
    result
}
