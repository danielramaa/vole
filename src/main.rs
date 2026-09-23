mod app;
mod config;
mod term;

use anyhow::{Result, bail};
use clap::{Parser, Subcommand};

use crate::app::{App, Screen};
use crate::config::Config;

/// Clean, analyze and maintain an Arch Linux system.
#[derive(Parser)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Clone, Copy)]
enum Command {
    /// Caches, pacman cache, journal and trash
    Clean,
    /// Orphaned packages and configs
    Orphans,
    /// Interactive disk explorer
    Analyze,
    /// Real-time system dashboard
    Status,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    if rustix::process::geteuid().is_root() {
        bail!(
            "vole must not be run as root; run it as your normal user (it asks for sudo when needed)"
        );
    }

    let config = Config::load()?;

    let screen = match cli.command {
        None => Screen::Menu,
        Some(Command::Clean) => Screen::Clean,
        Some(Command::Orphans) => Screen::Orphans,
        Some(Command::Analyze) => Screen::Analyze,
        Some(Command::Status) => Screen::Status,
    };

    let mut terminal = ratatui::init();
    let result = App::new(screen, config).run(&mut terminal);
    ratatui::restore();
    result
}
