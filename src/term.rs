use std::io::{self, stdout};
use std::process::{Command, ExitStatus};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use ratatui::DefaultTerminal;
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use signal_hook::consts::{SIGINT, SIGQUIT};

/// What happened while the TUI was paused.
pub struct Ran {
    /// The exit status of each command that ran, in order. Empty when the TUI couldn't be
    /// paused, since then nothing ran.
    pub statuses: Vec<io::Result<ExitStatus>>,
    /// The first error pausing the TUI or bringing it back.
    pub terminal: io::Result<()>,
}

/// Pauses the TUI, runs `commands` one after another in the normal terminal (so `sudo` can ask
/// for the password itself), then brings the TUI back.
///
/// The TUI is always brought back, even when pausing or a command fails, and the statuses are
/// kept even when bringing it back fails.
pub fn run_external(terminal: &mut DefaultTerminal, commands: &mut [Command]) -> Ran {
    // Ctrl+C at the password prompt goes to the whole foreground process group. Like
    // system(3), vole ignores it while it waits, so only the command stops.
    let paused = ignore_interrupts(true).and_then(|()| pause(terminal));
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
    let _ = ignore_interrupts(false);

    Ran {
        statuses,
        terminal: paused.and(resumed),
    }
}

/// Makes SIGINT and SIGQUIT do nothing to vole (`true`), or act as usual again (`false`).
/// vole's handler is replaced by the default on `exec`, so the commands it runs still get them.
fn ignore_interrupts(ignore: bool) -> io::Result<()> {
    static ACT: OnceLock<Arc<AtomicBool>> = OnceLock::new();
    let act = match ACT.get() {
        Some(act) => act,
        None => {
            let act = Arc::new(AtomicBool::new(true));
            for signal in [SIGINT, SIGQUIT] {
                signal_hook::flag::register_conditional_default(signal, Arc::clone(&act))?;
            }
            ACT.get_or_init(|| act)
        }
    };
    act.store(!ignore, Ordering::SeqCst);
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interrupts_can_be_ignored_while_a_command_runs() {
        ignore_interrupts(true).unwrap();
        signal_hook::low_level::raise(SIGINT).unwrap();
        // vole is still running, but a command it starts gets the default action.
        let status = Command::new("sh")
            .args(["-c", "kill -INT $$; exit 0"])
            .status()
            .unwrap();
        ignore_interrupts(false).unwrap();
        assert!(!status.success());
    }
}
