//! Reading the installed packages from pacman, as the user: root-owned things are only measured
//! through the official tools (ADR 0001).

use std::collections::{BTreeSet, HashSet};
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use anyhow::{Context, Result, bail};

use crate::size;

pub const PACMAN: &str = "/usr/bin/pacman";
const PACMAN_CONF: &str = "/usr/bin/pacman-conf";

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
    pub config_files: Vec<ConfigFile>,
}

/// A config file of a package (in its `backup` array). Removing the package with `-n` deletes
/// it even when it was changed, instead of keeping a `.pacsave` copy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigFile {
    pub path: String,
    pub state: FileState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileState {
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
    // pacman exits with 1 and lists nothing when no package matches. It may still print
    // warnings, e.g. about an option in pacman.conf it doesn't know.
    let none = output.status.code() == Some(1)
        && output.stdout.is_empty()
        && errors(&String::from_utf8_lossy(&output.stderr))
            .next()
            .is_none();
    if none {
        return Ok(Vec::new());
    }
    check(output)?;
    Ok(words(&output.stdout))
}

/// Every installed package (`pacman -Qi`).
pub fn installed() -> Result<Vec<Info>> {
    let output = query(&["-Qi"])?;
    check(&output)?;
    Ok(parse_info(&String::from_utf8_lossy(&output.stdout)))
}

/// Every file installed packages own, as `pacman -Ql` lists them: `<package> <path>` per line,
/// folders ending with `/`.
pub fn files() -> Result<String> {
    let output = query(&["-Ql"])?;
    check(&output)?;
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// What `pacman -Qi` says about the installed package `name`.
pub fn package_info(name: &str) -> Result<Info> {
    let output = query(&["-Qi", "--", name])?;
    check(&output)?;
    parse_info(&String::from_utf8_lossy(&output.stdout))
        .into_iter()
        .next()
        .with_context(|| format!("pacman -Qi said nothing about {name}"))
}

/// The installed packages that own `path` (`pacman -Qo`); none when no package does.
pub fn owners(path: &Path) -> Result<Vec<String>> {
    owner_names(&query(&[
        OsStr::new("-Qo"),
        OsStr::new("--"),
        path.as_os_str(),
    ])?)
}

fn owner_names(output: &Output) -> Result<Vec<String>> {
    let stderr = String::from_utf8_lossy(&output.stderr);
    let none = !output.status.success()
        && errors(&stderr).next().is_some()
        && errors(&stderr).all(|error| error.starts_with("No package owns"));
    if none {
        return Ok(Vec::new());
    }
    check(output)?;
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(owner)
        .collect())
}

/// The package a line of `pacman -Qo` names, e.g. `/usr/bin/ls is owned by coreutils 9.5-1`.
fn owner(line: &str) -> Option<String> {
    // The path may hold spaces; the name and version can't.
    let mut words = line.rsplitn(3, ' ');
    let _version = words.next()?;
    let name = words.next()?;
    words
        .next()?
        .ends_with(" is owned by")
        .then(|| name.to_string())
}

/// The config files of the packages `names` (`pacman -Qii`).
pub fn config_files(names: &[&str]) -> Result<Vec<Info>> {
    let mut args = vec!["-Qii", "--"];
    args.extend_from_slice(names);
    let output = query(&args)?;
    check(&output)?;
    Ok(parse_info(&String::from_utf8_lossy(&output.stdout)))
}

/// What `pacman -Rns` would remove when given `names`: them and the dependencies only they
/// need. (`--print` can't be combined with `-n`, which only changes what happens to config
/// files.)
pub fn would_remove(names: &[&str]) -> Result<BTreeSet<String>> {
    let mut args = vec!["-Rsp", "--print-format", "%n", "--"];
    args.extend_from_slice(names);
    let output = query(&args)?;
    check(&output)?;
    Ok(words(&output.stdout).into_iter().collect())
}

/// The names of every installed package (`pacman -Qq`).
pub fn installed_names() -> Result<HashSet<String>> {
    let output = query(&["-Qq"])?;
    check(&output)?;
    Ok(words(&output.stdout).into_iter().collect())
}

/// The folders pacman keeps the packages it downloads in (`pacman-conf CacheDir`).
pub fn cache_dirs() -> Result<Vec<PathBuf>> {
    let output = Command::new(PACMAN_CONF)
        .arg("CacheDir")
        .env("LC_ALL", "C")
        .output()
        .with_context(|| format!("running {PACMAN_CONF}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let said = errors(&stderr).next();
        let said = said.or_else(|| stderr.lines().find(|line| !line.trim().is_empty()));
        match said {
            Some(message) => bail!("pacman-conf: {}", message.trim()),
            None => bail!("pacman-conf failed ({})", output.status),
        }
    }
    let lines = output.stdout.split(|&b| b == b'\n');
    let dirs = lines
        .filter(|line| !line.is_empty())
        .map(|line| PathBuf::from(OsString::from_vec(line.to_vec())));
    Ok(dirs.collect())
}

/// Whether `name` is a valid package name: alphanumerics and `@._+-`, not starting with `-` or
/// `.` (makepkg's rule), so it can't be read as an option.
pub fn is_package_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with(['-', '.'])
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "@._+-".contains(c))
}

fn words(stdout: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(stdout)
        .split_whitespace()
        .map(String::from)
        .collect()
}

/// Runs a read-only pacman command as the user: in the C locale and without a terminal width,
/// so its output can be parsed.
fn query<S: AsRef<OsStr>>(args: &[S]) -> Result<Output> {
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
    match message(output) {
        message if message.is_empty() => bail!("pacman failed ({})", output.status),
        message => bail!("pacman: {message}"),
    }
}

/// pacman's error message on one row. When pacman explains the error in `::` lines (printed to
/// stdout, e.g. which package depends on one being removed), those, since they say more; else
/// its errors, without the warnings printed alongside.
fn message(output: &Output) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let explained: Vec<&str> = stdout
        .lines()
        .filter_map(|line| line.strip_prefix("::"))
        .collect();
    let stderr = String::from_utf8_lossy(&output.stderr);
    let errors: Vec<&str> = errors(&stderr).collect();
    let lines = if !explained.is_empty() {
        explained
    } else if !errors.is_empty() {
        errors
    } else {
        stderr.lines().collect()
    };
    lines
        .iter()
        .map(|line| line.trim())
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("; ")
}

/// pacman's errors, one per line, without the `error: ` in front.
fn errors(stderr: &str) -> impl Iterator<Item = &str> {
    stderr
        .lines()
        .filter_map(|line| line.strip_prefix("error: "))
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
        config_files: values("Backup Files")
            .iter()
            .filter_map(|line| config_file(line))
            .collect(),
    })
}

/// Reads e.g. `/etc/pacman.conf [modified]`. A missing file is left out: nothing to remove.
fn config_file(line: &str) -> Option<ConfigFile> {
    let (path, state) = line.rsplit_once(' ')?;
    let state = match state {
        "[unmodified]" => FileState::Unmodified,
        "[modified]" => FileState::Modified,
        "[missing]" => return None,
        // `[unreadable]`, or `[unknown]` for any other error.
        _ => FileState::Unreadable,
    };
    Some(ConfigFile {
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
        let file = |path: &str, state| ConfigFile {
            path: path.into(),
            state,
        };
        assert_eq!(
            infos[0].config_files,
            [
                file("/etc/pacman.conf", FileState::Modified),
                file("/etc/makepkg.conf", FileState::Unmodified),
                file("/etc/makepkg.conf.d/my conf.conf", FileState::Unreadable),
            ]
        );
        assert!(infos[1].config_files.is_empty());
    }

    #[test]
    fn no_orphans_is_not_an_error() {
        assert!(orphan_names(&output(1, "", "")).unwrap().is_empty());
        assert_eq!(orphan_names(&output(0, "a\nb\n", "")).unwrap(), ["a", "b"]);
        assert!(orphan_names(&output(1, "", "error: could not open database")).is_err());
        // An option pacman doesn't know in pacman.conf only gets a warning.
        let warning = "warning: config file /etc/pacman.conf, line 107: \
                       directive 'Foo' in section 'options' not recognized.\n";
        assert!(orphan_names(&output(1, "", warning)).unwrap().is_empty());
        let failed = output(1, "", &format!("{warning}error: could not open database\n"));
        assert_eq!(
            orphan_names(&failed).unwrap_err().to_string(),
            "pacman: could not open database"
        );
    }

    #[test]
    fn reads_who_owns_a_path() {
        let owned = output(
            0,
            "/usr/share/icons/ is owned by adwaita-icon-theme 50.0-1\n\
             /usr/share/icons/ is owned by breeze 6.7.5-1.1\n",
            "",
        );
        assert_eq!(
            owner_names(&owned).unwrap(),
            ["adwaita-icon-theme", "breeze"]
        );
        let spaced = output(0, "/opt/My App/run is owned by my-app 1.0-1\n", "");
        assert_eq!(owner_names(&spaced).unwrap(), ["my-app"]);
        let none = output(1, "", "error: No package owns /var/lib/docker\n");
        assert!(owner_names(&none).unwrap().is_empty());
        let failed = output(1, "", "error: could not open database\n");
        assert!(owner_names(&failed).is_err());
    }

    #[test]
    fn package_names_cannot_be_options() {
        for name in [
            "zstd",
            "python-pkg_resources",
            "lib32-libcap",
            "gtk+3",
            "foo@bar.baz",
        ] {
            assert!(is_package_name(name), "{name}");
        }
        for name in [
            "",
            "-Syu",
            "--root=/tmp",
            ".hidden",
            "a b",
            "a/b",
            "caf\u{e9}",
        ] {
            assert!(!is_package_name(name), "{name}");
        }
    }

    #[test]
    fn error_messages_fit_on_one_row() {
        let failed = output(
            1,
            "a\n:: removing b breaks dependency 'b' required by a\n",
            "error: failed to prepare transaction (could not satisfy dependencies)\n",
        );
        assert_eq!(
            check(&failed).unwrap_err().to_string(),
            "pacman: removing b breaks dependency 'b' required by a"
        );
        let unexplained = output(
            1,
            "",
            "error: target not found: x\nerror: target not found: y\n",
        );
        assert_eq!(
            check(&unexplained).unwrap_err().to_string(),
            "pacman: target not found: x; target not found: y"
        );
        assert_eq!(
            check(&output(1, "", "")).unwrap_err().to_string(),
            "pacman failed (exit status: 1)"
        );
    }
}
