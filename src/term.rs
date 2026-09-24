use std::io::{self, stdout};
use std::process::{Command, ExitStatus};

use ratatui::DefaultTerminal;
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};

/// Pauses the TUI, runs `commands` one after another in the normal terminal (so `sudo` can ask
/// for the password itself), then brings the TUI back. Returns each command's exit status.
///
/// The TUI is always brought back, even when pausing or a command fails.
pub fn run_external(
    terminal: &mut DefaultTerminal,
    commands: &mut [Command],
) -> io::Result<Vec<io::Result<ExitStatus>>> {
    let paused = pause(terminal);
    let statuses = match &paused {
        Ok(()) => commands
            .iter_mut()
            .map(|command| {
                println!("vole: running {}", describe(command));
                command.status()
            })
            .collect(),
        Err(_) => Vec::new(),
    };
    let resumed = resume(terminal);

    paused?;
    resumed?;
    Ok(statuses)
}

/// The command line, as shown to the user before it runs.
pub fn describe(command: &Command) -> String {
    let mut line = command.get_program().to_string_lossy().into_owned();
    for arg in command.get_args() {
        line.push(' ');
        line.push_str(&arg.to_string_lossy());
    }
    line
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
