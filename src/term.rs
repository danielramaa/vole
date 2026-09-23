use std::io::{self, stdout};
use std::process::{Command, ExitStatus};

use ratatui::DefaultTerminal;
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};

/// Pauses the TUI, runs `command` in the normal terminal (so `sudo` can ask for the
/// password itself), then brings the TUI back.
///
/// The TUI is always brought back, even when pausing or the command fails.
#[allow(dead_code)] // first used by `clean` in M2
pub fn run_external(
    terminal: &mut DefaultTerminal,
    command: &mut Command,
) -> io::Result<ExitStatus> {
    let paused = pause(terminal);
    let status = match &paused {
        Ok(()) => command.status(),
        Err(_) => Err(io::Error::other("terminal could not be paused")),
    };
    let resumed = resume(terminal);

    paused?;
    resumed?;
    status
}

fn pause(terminal: &mut DefaultTerminal) -> io::Result<()> {
    disable_raw_mode()?;
    execute!(stdout(), LeaveAlternateScreen)?;
    terminal.show_cursor()
}

/// Runs every step even if an earlier one fails, and reports the first error.
fn resume(terminal: &mut DefaultTerminal) -> io::Result<()> {
    let steps = [
        execute!(stdout(), EnterAlternateScreen),
        enable_raw_mode(),
        terminal.hide_cursor(),
        terminal.clear(),
    ];
    steps.into_iter().collect()
}
