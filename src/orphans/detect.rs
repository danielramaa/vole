//! Reading what is installed outside pacman, as the user: programs on `$PATH`, desktop entries,
//! Flatpaks and AppImages.

use std::env;
use std::fs;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const FLATPAK: &str = "/usr/bin/flatpak";

/// How long `flatpak list` may take. Past that, the scan goes on without it: Flatpak's apps are
/// still known by the desktop entries it exports.
const FLATPAK_TIMEOUT: Duration = Duration::from_secs(5);

/// Where user programs live even when they aren't on `$PATH` when vole runs.
const USER_BIN: &[&str] = &[".local/bin", ".cargo/bin"];

/// Where AppImages are usually kept, relative to the home directory.
const APPIMAGE_DIRS: &[&str] = &[
    "Applications",
    "AppImages",
    ".local/bin",
    "bin",
    "Desktop",
    "Downloads",
    "opt",
];

/// What is installed outside pacman.
#[derive(Debug, Default)]
pub struct OutsidePacman {
    /// The folders on `$PATH`, resolved (see [`path_dirs`]).
    pub path_dirs: Vec<PathBuf>,
    /// The programs in them.
    pub programs: Vec<PathBuf>,
    pub desktop_entries: Vec<DesktopEntry>,
    /// Flatpak app ids.
    pub flatpaks: Vec<String>,
    pub appimages: Vec<PathBuf>,
}

impl OutsidePacman {
    pub fn read(home: &Path) -> Self {
        let path_dirs = path_dirs(home);
        Self {
            programs: programs(&path_dirs),
            desktop_entries: desktop_entries(home),
            flatpaks: flatpaks(),
            appimages: appimages(home),
            path_dirs,
        }
    }
}

/// The folders on `$PATH`, plus `~/.local/bin` and `~/.cargo/bin`: resolved, existing, and
/// each once.
pub fn path_dirs(home: &Path) -> Vec<PathBuf> {
    let on_path = env::var_os("PATH")
        .map(|path| env::split_paths(&path).collect::<Vec<_>>())
        .unwrap_or_default();
    let user = USER_BIN.iter().map(|dir| home.join(dir));
    let mut dirs: Vec<PathBuf> = Vec::new();
    for dir in on_path.into_iter().chain(user) {
        if let Ok(dir) = fs::canonicalize(dir)
            && dir.is_dir()
            && !dirs.contains(&dir)
        {
            dirs.push(dir);
        }
    }
    dirs
}

/// The programs in `dirs`: executable files, or symlinks to them.
pub fn programs(dirs: &[PathBuf]) -> Vec<PathBuf> {
    let mut programs = Vec::new();
    for dir in dirs {
        for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if fs::metadata(&path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            {
                programs.push(path);
            }
        }
    }
    programs
}

/// Whether `program`, as a desktop entry's Exec line names it, exists: an absolute path, or a
/// name found in one of `dirs`.
pub fn program_exists(program: &str, dirs: &[PathBuf]) -> bool {
    if program.contains('/') {
        return Path::new(program).exists();
    }
    dirs.iter().any(|dir| dir.join(program).exists())
}

/// A desktop entry: the `.desktop` file that puts a program in the application menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopEntry {
    pub path: PathBuf,
    /// The file's name without `.desktop`, e.g. `org.kde.dolphin`.
    pub id: String,
    /// The window class it says the program's windows have, e.g. `Code`.
    pub wm_class: Option<String>,
    /// The program its Exec line runs, as written there.
    pub program: Option<String>,
    /// `Hidden=true`: the entry is deleted, so it says nothing is installed.
    pub hidden: bool,
}

/// The desktop entries in the system's and the user's application folders, including the ones
/// Flatpak exports.
pub fn desktop_entries(home: &Path) -> Vec<DesktopEntry> {
    let dirs = [
        PathBuf::from("/usr/share/applications"),
        PathBuf::from("/usr/local/share/applications"),
        PathBuf::from("/var/lib/flatpak/exports/share/applications"),
        home.join(".local/share/applications"),
        home.join(".local/share/flatpak/exports/share/applications"),
    ];
    let mut entries = Vec::new();
    for dir in dirs {
        for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "desktop")
                && let Ok(text) = fs::read_to_string(&path)
            {
                entries.push(parse_desktop_entry(path, &text));
            }
        }
    }
    entries
}

/// Reads the `[Desktop Entry]` group of a desktop file.
pub fn parse_desktop_entry(path: PathBuf, text: &str) -> DesktopEntry {
    let id = path
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let mut entry = DesktopEntry {
        path,
        id,
        wm_class: None,
        program: None,
        hidden: false,
    };
    let mut in_entry = false;
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
        } else if !in_entry {
            continue;
        } else if let Some(class) = line.strip_prefix("StartupWMClass=") {
            entry.wm_class = Some(class.trim().to_string());
        } else if let Some(exec) = line.strip_prefix("Exec=") {
            entry.program = exec_program(exec);
        } else if let Some(hidden) = line.strip_prefix("Hidden=") {
            entry.hidden = hidden.trim() == "true";
        }
    }
    entry
}

/// The program an Exec line runs: `env FOO=1 /opt/app/app --flag %U` → `/opt/app/app`.
fn exec_program(exec: &str) -> Option<String> {
    let mut words = exec_words(exec).into_iter();
    let mut word = words.next()?;
    if word == "env" {
        // env's own options and variables come first: env -u FOO BAR=1 app.
        loop {
            word = words.next()?;
            if word == "-u" || word == "--unset" {
                words.next()?;
            } else if !(word.starts_with('-') || word.contains('=')) {
                break;
            }
        }
    }
    Some(word)
}

/// The words of an Exec line, where double quotes keep spaces in a word.
fn exec_words(exec: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quoted = false;
    let mut chars = exec.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => quoted = !quoted,
            '\\' if quoted => word.extend(chars.next()),
            c if c.is_whitespace() && !quoted => {
                if !word.is_empty() {
                    words.push(std::mem::take(&mut word));
                }
            }
            c => word.push(c),
        }
    }
    if !word.is_empty() {
        words.push(word);
    }
    words
}

/// The Flatpak apps installed, by id (`flatpak list`); none when Flatpak isn't, or doesn't answer
/// in time.
pub fn flatpaks() -> Vec<String> {
    if !Path::new(FLATPAK).exists() {
        return Vec::new();
    }
    let mut command = Command::new(FLATPAK);
    command
        .args(["list", "--app", "--columns=application"])
        .env("LC_ALL", "C");
    run_for_a_while(command, FLATPAK_TIMEOUT)
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(String::from)
        .collect()
}

/// What `command` prints, if it succeeds within `timeout`; it is killed when it takes longer.
fn run_for_a_while(mut command: Command, timeout: Duration) -> Option<String> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    // Read meanwhile, so a full pipe can't keep it from finishing.
    let mut stdout = child.stdout.take()?;
    let reader = thread::spawn(move || {
        let mut text = String::new();
        stdout.read_to_string(&mut text).ok().map(|_| text)
    });
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < timeout => thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    let text = reader.join().ok()??;
    status.success().then_some(text)
}

/// The AppImages in the folders they are usually kept in.
pub fn appimages(home: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for dir in APPIMAGE_DIRS {
        for entry in fs::read_dir(home.join(dir)).into_iter().flatten().flatten() {
            let path = entry.path();
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            if name.to_lowercase().ends_with(".appimage") && path.is_file() {
                found.push(path);
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testdir::TestDir;
    use std::fs::Permissions;

    #[test]
    fn finds_programs_that_can_run() {
        let dir = TestDir::new();
        let program = dir.file("bin/tool", 10);
        fs::set_permissions(&program, Permissions::from_mode(0o755)).unwrap();
        dir.file("bin/readme", 10);
        let linked = dir.symlink("bin/linked", &program);
        let bin = dir.path().join("bin");
        let mut found = programs(std::slice::from_ref(&bin));
        found.sort();
        assert_eq!(found, [linked, program]);
        assert!(program_exists("tool", std::slice::from_ref(&bin)));
        assert!(!program_exists("readme-not-there", &[bin]));
    }

    #[test]
    fn reads_desktop_entries() {
        let text = "\
[Desktop Entry]
Name=Visual Studio Code
Exec=env GDK_BACKEND=x11 /usr/share/code/code --unity-launch %F
StartupWMClass=Code

[Desktop Action new-empty-window]
Exec=/usr/share/code/code --new-window %F
StartupWMClass=Other
";
        let entry = parse_desktop_entry("/usr/share/applications/code.desktop".into(), text);
        assert_eq!(entry.id, "code");
        assert_eq!(entry.wm_class.as_deref(), Some("Code"));
        assert_eq!(entry.program.as_deref(), Some("/usr/share/code/code"));
        assert_eq!(
            exec_program("\"/opt/My App/app\" %U").as_deref(),
            Some("/opt/My App/app")
        );
        assert_eq!(exec_program("").as_deref(), None);
        assert_eq!(
            exec_program("env -u GTK_IM_MODULE -i LANG=C app %U").as_deref(),
            Some("app")
        );
        let hidden = parse_desktop_entry(
            "/home/me/.local/share/applications/gone.desktop".into(),
            "[Desktop Entry]\nHidden=true\nExec=gone\n",
        );
        assert!(hidden.hidden);
    }

    #[test]
    fn gives_up_on_commands_that_take_too_long() {
        let mut quick = Command::new("sh");
        quick.args(["-c", "echo com.example.App"]);
        assert_eq!(
            run_for_a_while(quick, Duration::from_secs(5)).as_deref(),
            Some("com.example.App\n")
        );
        let mut slow = Command::new("sleep");
        slow.arg("10");
        let started = Instant::now();
        assert_eq!(run_for_a_while(slow, Duration::from_millis(200)), None);
        assert!(started.elapsed() < Duration::from_secs(5));
        let mut failing = Command::new("false");
        failing.arg("");
        assert_eq!(run_for_a_while(failing, Duration::from_secs(5)), None);
    }

    #[test]
    fn finds_appimages() {
        let home = TestDir::new();
        let app = home.file("Applications/Obsidian-1.4.16.AppImage", 10);
        home.file("Downloads/notes.txt", 10);
        let other = home.file("Downloads/tool.appimage", 10);
        let mut found = appimages(home.path());
        found.sort();
        assert_eq!(found, [app, other]);
    }
}
