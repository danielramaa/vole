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
    /// Installed as a dependency of another package, not explicitly.
    pub dependency: bool,
    /// The installed packages that depend on this one. pacman works this out with provides and
    /// versions, so it is exact.
    pub required_by: Vec<String>,
    /// The installed packages that list this one as an optional dependency.
    pub optional_for: Vec<String>,
    /// The package's config files; only `pacman -Qii` lists them.
    pub backup: Vec<Backup>,
}

/// A config file of a package (in its `backup` array). Removing the package with `-n` deletes
/// it even when it was changed, instead of keeping a `.pacsave` copy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Backup {
    pub path: String,
    pub state: BackupState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackupState {
    Unmodified,
    /// Changed since it was installed.
    Modified,
    /// The user can't read it, so whether it was changed is unknown.
    Unreadable,
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

/// The config files of the packages `names` (`pacman -Qii`).
pub fn backup_files(names: &[&str]) -> Result<Vec<Info>> {
    let mut args = vec!["-Qii", "--"];
    args.extend_from_slice(names);
    let output = query(&args)?;
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
    let values = |name: &str| {
        fields
            .iter()
            .find(|(field, _)| *field == name)
            .map_or(&[][..], |(_, values)| values.as_slice())
    };
    let first = |name: &str| values(name).first().copied().unwrap_or("");
    let names = |name: &str| -> Vec<String> {
        match values(name) {
            ["None"] => Vec::new(),
            lines => lines
                .iter()
                .flat_map(|line| line.split_whitespace())
                .map(String::from)
                .collect(),
        }
    };
    let name = first("Name");
    if name.is_empty() {
        return None;
    }
    Some(Info {
        name: name.to_string(),
        size: size::parse(first("Installed Size")).unwrap_or(0),
        // Anything else, even an unknown reason, counts as explicit: never an orphan.
        dependency: first("Install Reason") == "Installed as a dependency for another package",
        required_by: names("Required By"),
        optional_for: names("Optional For"),
        backup: values("Backup Files")
            .iter()
            .filter_map(|line| backup(line))
            .collect(),
    })
}

/// Reads e.g. `/etc/pacman.conf [modified]`. A missing file is left out: nothing to remove.
fn backup(line: &str) -> Option<Backup> {
    let (path, state) = line.rsplit_once(' ')?;
    let state = match state {
        "[unmodified]" => BackupState::Unmodified,
        "[modified]" => BackupState::Modified,
        "[missing]" => return None,
        // `[unreadable]`, or `[unknown]` for any other error.
        _ => BackupState::Unreadable,
    };
    Some(Backup {
        path: path.trim_end().to_string(),
        state,
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
        assert!(infos[0].dependency && !infos[1].dependency);
        assert!(infos[0].required_by.is_empty() && infos[0].optional_for.is_empty());
        // Wrapped onto a second line.
        assert_eq!(infos[1].required_by, ["python-lexicon", "python-paramiko"]);
    }

    #[test]
    fn reads_config_files_and_whether_they_changed() {
        let qii = "\
Name            : pacman
Backup Files    : /etc/pacman.conf [modified]
                  /etc/makepkg.conf [unmodified]
                  /etc/makepkg.conf.d/my conf.conf [unreadable]
                  /etc/gone.conf [missing]
Extended Data   : pkgtype=pkg

Name            : zstd
Backup Files    : None
";
        let infos = parse_info(qii);
        let backup = |path: &str, state| Backup {
            path: path.into(),
            state,
        };
        assert_eq!(
            infos[0].backup,
            [
                backup("/etc/pacman.conf", BackupState::Modified),
                backup("/etc/makepkg.conf", BackupState::Unmodified),
                backup("/etc/makepkg.conf.d/my conf.conf", BackupState::Unreadable),
            ]
        );
        assert!(infos[1].backup.is_empty());
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
