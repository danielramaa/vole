use std::io::{self, stdout};
use std::process::{Command, ExitStatus};

use ratatui::DefaultTerminal;
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};

/// Pauses the TUI, runs `command` in the normal terminal (so `sudo` can ask for the
/// password itself), then brings the TUI back.
#[allow(dead_code)] // first used by `clean` in M2
pub fn run_external(
    terminal: &mut DefaultTerminal,
    command: &mut Command,
) -> io::Result<ExitStatus> {
    disable_raw_mode()?;
    execute!(stdout(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    let status = command.status();

    execute!(stdout(), EnterAlternateScreen)?;
    enable_raw_mode()?;
    terminal.hide_cursor()?;
    terminal.clear()?;

    status
}
