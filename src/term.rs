use std::io::{self, Write, stdin, stdout};
use std::process::{Command, ExitStatus};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread;
use std::time::Duration;

use ratatui::DefaultTerminal;
use ratatui::crossterm::event;
use ratatui::crossterm::execute;
use ratatui::crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use rustix::termios::{QueueSelector, tcflush};
use signal_hook::consts::{SIGINT, SIGQUIT};

/// Root commands always go through sudo, called by absolute path (ADR 0001).
const SUDO: &str = "/usr/bin/sudo";

/// A command that runs `program` as root through sudo, which asks for the password itself.
pub fn as_root(program: &str) -> Command {
    let mut command = Command::new(SUDO);
    command.arg(program);
    command
}

/// Runs root commands, all under one pause of the TUI.
pub type RunRoot<'a> = dyn FnMut(&mut [Command]) -> Ran + 'a;

/// What happened while the TUI was paused.
pub struct Ran {
    /// The exit status of each command that ran, in order. Empty when the TUI couldn't be
    /// paused, since then nothing ran.
    pub statuses: Vec<io::Result<ExitStatus>>,
    /// The first error pausing the TUI or bringing it back.
    pub terminal: io::Result<()>,
}

impl Ran {
    /// Why the command at `index`, called `what` here, didn't succeed; `None` when it did.
    pub fn error(&self, index: usize, what: &str) -> Option<String> {
        match self.statuses.get(index) {
            Some(Ok(status)) if status.success() => None,
            Some(Ok(status)) => Some(format!("{what} failed ({status})")),
            Some(Err(err)) => Some(format!("{what} could not run: {err}")),
            None => Some(match &self.terminal {
                Err(err) if self.statuses.is_empty() => {
                    format!("{what} did not run: the terminal could not be paused: {err}")
                }
                _ => format!("{what} did not run"),
            }),
        }
    }
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
    let statuses: Vec<io::Result<ExitStatus>> = match &paused {
        Ok(()) => {
            discard_typeahead();
            commands
                .iter_mut()
                .map(|command| {
                    println!("vole: running {}", describe(command));
                    command.status()
                })
                .collect()
        }
        Err(_) => Vec::new(),
    };
    // Coming back would hide why a command failed (a wrong password, not in sudoers, …).
    if !statuses
        .iter()
        .all(|s| s.as_ref().is_ok_and(ExitStatus::success))
    {
        wait_for_enter();
    }
    let resumed = resume(terminal);
    let _ = ignore_interrupts(false);
    // Keys typed while the TUI was paused are not meant for it.
    if resumed.is_ok() {
        while event::poll(Duration::ZERO).unwrap_or(false) {
            let _ = event::read();
        }
    }

    Ran {
        statuses,
        terminal: paused.and(resumed),
    }
}

/// How long after the TUI pauses a key can't yet be a reaction to it.
const TYPEAHEAD: Duration = Duration::from_millis(200);

/// Throws away the keys typed before the TUI paused, or too soon after to answer what comes
/// next (a quick Enter after y). They were meant for vole, and would otherwise go into sudo's
/// password or answer a command's question, such as pacman's [Y/n].
fn discard_typeahead() {
    thread::sleep(TYPEAHEAD);
    let _ = tcflush(stdin(), QueueSelector::IFlush);
}

fn wait_for_enter() {
    print!("vole: a command failed; its output is above. Press Enter to go back to vole.");
    let _ = stdout().flush();
    let _ = io::stdin().read_line(&mut String::new());
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
    use std::os::unix::process::ExitStatusExt;

    #[test]
    fn says_why_a_command_did_not_succeed() {
        let ran = Ran {
            statuses: vec![
                Ok(ExitStatus::from_raw(0)),
                Ok(ExitStatus::from_raw(1 << 8)),
                Err(io::Error::other("no such file")),
            ],
            terminal: Ok(()),
        };
        assert_eq!(ran.error(0, "a"), None);
        assert_eq!(ran.error(1, "b").unwrap(), "b failed (exit status: 1)");
        assert_eq!(ran.error(2, "c").unwrap(), "c could not run: no such file");
        assert_eq!(ran.error(3, "d").unwrap(), "d did not run");
        let not_paused = Ran {
            statuses: Vec::new(),
            terminal: Err(io::Error::other("no tty")),
        };
        assert_eq!(
            not_paused.error(0, "a").unwrap(),
            "a did not run: the terminal could not be paused: no tty"
        );
    }

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
