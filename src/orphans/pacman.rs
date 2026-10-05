//! Reading the installed packages from pacman, as the user: root-owned things are only measured
//! through the official tools (ADR 0001).

use std::process::{Command, Output};

use anyhow::{Context, Result, bail};

use crate::size;

pub const PACMAN: &str = "/usr/bin/pacman";

/// What `pacman -Qi` says about one installed package.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Info {
    pub name: String,
    /// Installed size in bytes, as rounded by pacman.
    pub size: u64,
}

/// The packages `pacman -Qdtq` lists: installed as dependencies, and neither required nor
/// optionally required by any installed package.
pub fn orphans() -> Result<Vec<String>> {
    orphan_names(&query(&["-Qdtq"])?)
}

fn orphan_names(output: &Output) -> Result<Vec<String>> {
    // pacman exits with 1, saying nothing, when no package matches.
    let none =
        output.status.code() == Some(1) && output.stdout.is_empty() && output.stderr.is_empty();
    if none {
        return Ok(Vec::new());
    }
    check(output)?;
    Ok(String::from_utf8_lossy(&output.stdout)
        .split_whitespace()
        .map(String::from)
        .collect())
}

/// Every installed package (`pacman -Qi`).
pub fn installed() -> Result<Vec<Info>> {
    let output = query(&["-Qi"])?;
    check(&output)?;
    Ok(parse_info(&String::from_utf8_lossy(&output.stdout)))
}

/// Runs a read-only pacman command as the user: in the C locale and without a terminal width,
/// so its output can be parsed.
fn query(args: &[&str]) -> Result<Output> {
    Command::new(PACMAN)
        .args(["--color", "never"])
        .args(args)
        .env("LC_ALL", "C")
        .env_remove("COLUMNS")
        .output()
        .with_context(|| format!("running {PACMAN}"))
}

fn check(output: &Output) -> Result<()> {
    if output.status.success() {
        return Ok(());
    }
    bail!("pacman failed: {}", message(&output.stderr))
}

/// pacman's error message on one row.
fn message(stderr: &[u8]) -> String {
    String::from_utf8_lossy(stderr)
        .lines()
        .map(|line| line.trim().trim_start_matches("error: "))
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Reads `pacman -Qi` output: a block of `Field : value` lines per package. A value pacman
/// wraps, or lists one entry per line, continues on lines that start with spaces.
pub fn parse_info(text: &str) -> Vec<Info> {
    let mut infos = Vec::new();
    let mut fields: Vec<(&str, Vec<&str>)> = Vec::new();
    // The blank line at the end closes the last block.
    for line in text.lines().chain([""]) {
        if line.trim().is_empty() {
            infos.extend(info(&fields));
            fields.clear();
        } else if line.starts_with(char::is_whitespace) {
            if let Some((_, values)) = fields.last_mut() {
                values.push(line.trim());
            }
        } else if let Some((field, value)) = line.split_once(':') {
            fields.push((field.trim(), vec![value.trim()]));
        }
    }
    infos
}

fn info(fields: &[(&str, Vec<&str>)]) -> Option<Info> {
    let first = |name: &str| {
        fields
            .iter()
            .find(|(field, _)| *field == name)
            .and_then(|(_, values)| values.first().copied())
            .unwrap_or("")
    };
    let name = first("Name");
    if name.is_empty() {
        return None;
    }
    Some(Info {
        name: name.to_string(),
        size: size::parse(first("Installed Size")).unwrap_or(0),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt;
    use std::process::ExitStatus;

    /// Two packages as `pacman -Qi` prints them, the first wrapped to a narrow terminal.
    const QI: &str = "\
Installed From  : extra
Name            : python-paramiko
Version         : 5.0.0-1
Description     : Python module that implements the SSH2 protocol: a long
                  description, wrapped
Depends On      : python-bcrypt  python-cryptography
                  python-pynacl  python-invoke
Required By     : None
Optional For    : None
Installed Size  : 2.10 MiB
Install Reason  : Installed as a dependency for another package

Name            : python-invoke
Required By     : python-lexicon
                  python-paramiko
Optional For    : None
Installed Size  : 1073.91 KiB
Install Reason  : Explicitly installed
";

    fn output(code: i32, stdout: &str, stderr: &str) -> Output {
        Output {
            status: ExitStatus::from_raw(code << 8),
            stdout: stdout.into(),
            stderr: stderr.into(),
        }
    }

    #[test]
    fn reads_installed_packages() {
        let infos = parse_info(QI);
        let names: Vec<&str> = infos.iter().map(|info| info.name.as_str()).collect();
        assert_eq!(names, ["python-paramiko", "python-invoke"]);
        assert_eq!(infos[0].size, size::parse("2.10 MiB").unwrap());
        assert_eq!(infos[1].size, size::parse("1073.91 KiB").unwrap());
    }

    #[test]
    fn no_orphans_is_not_an_error() {
        assert!(orphan_names(&output(1, "", "")).unwrap().is_empty());
        assert_eq!(orphan_names(&output(0, "a\nb\n", "")).unwrap(), ["a", "b"]);
        assert!(orphan_names(&output(1, "", "error: could not open database")).is_err());
    }

    #[test]
    fn error_messages_fit_on_one_row() {
        let stderr = b"error: failed to prepare transaction (could not satisfy dependencies)\n\
                       :: removing b breaks dependency 'b' required by a\n";
        assert_eq!(
            message(stderr),
            "failed to prepare transaction (could not satisfy dependencies) \
             :: removing b breaks dependency 'b' required by a"
        );
    }
}
