//! Root-owned Categories, measured only through the official tools (ADR 0001).

use std::fs;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};

use super::plan::{Category, Item, JOURNALCTL, Kind, PACCACHE, RootCommand};
use crate::size;

pub fn pacman(keep: u32) -> Category {
    let mut category = Category::new("Pacman cache");
    if !Path::new(PACCACHE).exists() {
        category.note = Some("paccache not found; install pacman-contrib".into());
        return category;
    }
    // `-k<n>` covers uninstalled packages too, and paccache can't leave them out. So that
    // Item says so, and the uninstalled Item only gets what `-k<n>` leaves of them. It then
    // can't be selected without the first one (`RootCommand::requires`), and runs after it,
    // so what is removed is always what the selected sizes add up to.
    let sizes = [
        (
            format!("older versions of every package, installed or not (keeping {keep})"),
            RootCommand::PacmanKeep(keep),
            paccache_dry_run(&[format!("-k{keep}")]),
        ),
        (
            "remaining versions of uninstalled packages".into(),
            RootCommand::PacmanUninstalled,
            paccache_dry_run(&["-u".into(), "-k0".into()]).and_then(|all| {
                let trimmed = paccache_dry_run(&["-u".into(), format!("-k{keep}")])?;
                Ok(all.saturating_sub(trimmed))
            }),
        ),
    ];
    let mut errors: Vec<String> = Vec::new();
    for (label, command, size) in sizes {
        match size {
            Ok(0) => {}
            Ok(size) => category
                .items
                .push(Item::new(label, Kind::Root(command), size)),
            Err(err) => {
                let err = format!("{err:#}");
                if !errors.contains(&err) {
                    errors.push(err);
                }
            }
        }
    }
    if !errors.is_empty() {
        category.note = Some(errors.join("; "));
    }
    category
}

/// What `paccache -d <selection>` says it would free.
fn paccache_dry_run(selection: &[String]) -> Result<u64> {
    let mut args = vec!["-d".to_string(), "--nocolor".to_string()];
    args.extend_from_slice(selection);
    let output = run(PACCACHE, &args)?;
    parse_paccache(&output).context("could not read paccache's dry run")
}

pub fn journal(max_mb: u32) -> Category {
    let mut category = Category::new("Journal");
    let usage = run(JOURNALCTL, &["--disk-usage".to_string()]).and_then(|output| {
        parse_journal_usage(&output).context("could not read journalctl --disk-usage")
    });
    // An estimate: `--disk-usage` counts the active files too, which the vacuum only reaches
    // because M2 rotates them into archived files first (`--rotate --vacuum-size`).
    match usage {
        Ok(usage) => {
            if !can_read_system_journal(Path::new(JOURNAL_DIR)) {
                category.note = Some("only the journal files you can read are counted".into());
            }
            let freed = usage.saturating_sub(u64::from(max_mb) * size::MIB);
            if freed > 0 {
                let label = format!("shrink from about {} to {max_mb} MiB", size::format(usage));
                category.items.push(Item::new(
                    label,
                    Kind::Root(RootCommand::JournalVacuum(max_mb)),
                    freed,
                ));
            }
        }
        Err(err) => category.note = Some(format!("{err:#}")),
    }
    category
}

const JOURNAL_DIR: &str = "/var/log/journal";

/// Whether the user can list every persistent journal folder (users outside the `wheel`, `adm`
/// and `systemd-journal` groups can't, and then `--disk-usage` only counts their own files).
fn can_read_system_journal(dir: &Path) -> bool {
    let Ok(entries) = fs::read_dir(dir) else {
        // No persistent journal, or not even this folder is readable.
        return !dir.exists();
    };
    entries
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|t| t.is_dir()))
        .all(|entry| fs::read_dir(entry.path()).is_ok())
}

/// Runs a measuring command as the user, in the C locale so its output can be parsed.
fn run(program: &str, args: &[String]) -> Result<String> {
    let output = Command::new(program)
        .args(args)
        .env("LC_ALL", "C")
        .output()
        .with_context(|| format!("running {program}"))?;
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    if !output.status.success() {
        // Notes are shown on one row, so only the first line of the tool's message.
        bail!("{program} failed: {}", first_line(&text));
    }
    Ok(text)
}

fn first_line(text: &str) -> &str {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("")
}

/// Reads e.g. `==> finished dry run: 5 candidates (disk space saved: 164.84 MiB)`.
pub fn parse_paccache(output: &str) -> Option<u64> {
    if let Some(rest) = output.split("disk space saved:").nth(1) {
        return size::parse(rest.split(')').next()?);
    }
    output.contains("no candidate packages found").then_some(0)
}

/// Reads e.g. `Archived and active journals take up 1.2G in the file system.`
pub fn parse_journal_usage(output: &str) -> Option<u64> {
    let rest = output.split("take up ").nth(1)?;
    size::parse(rest.split_whitespace().next()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_paccache_dry_runs() {
        let output = "==> finished dry run: 5 candidates (disk space saved: 164.84 MiB)\n";
        assert_eq!(parse_paccache(output), size::parse("164.84 MiB"));
        assert_eq!(
            parse_paccache("==> no candidate packages found for pruning"),
            Some(0)
        );
        assert_eq!(parse_paccache("something else"), None);
    }

    #[test]
    fn a_readable_or_missing_journal_folder_is_fully_counted() {
        let dir = crate::testdir::TestDir::new();
        dir.dir("machine-id");
        assert!(can_read_system_journal(dir.path()));
        assert!(can_read_system_journal(&dir.path().join("missing")));
    }

    #[test]
    fn errors_keep_only_the_first_line() {
        assert_eq!(
            first_line("\n  error: no cache dir\nusage: paccache ...\n"),
            "error: no cache dir"
        );
        assert_eq!(first_line(""), "");
    }

    #[test]
    fn reads_journal_usage() {
        let output = "Archived and active journals take up 1.5G in the file system.\n";
        assert_eq!(parse_journal_usage(output), Some(1536 * size::MIB));
        assert_eq!(parse_journal_usage("No journal files were found."), None);
    }
}
