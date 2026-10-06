//! Owners: what installed thing accounts for a file or folder in the home directory.
//!
//! A folder belongs to whatever it is named after. Its name is turned into the names it may go
//! by ([`name_keys`]: `dolphinstaterc` → `dolphin`), and those are looked up among the names of
//! what is installed. Matching generously is the safe side: a folder wrongly thought owned is only
//! not proposed, while one wrongly thought orphaned would be.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use super::detect::{self, OutsidePacman};
use crate::pacman::Info;

/// Something installed that accounts for a config or data folder.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Owner {
    /// A pacman package.
    Package(String),
    /// A Flatpak app, by id.
    Flatpak(String),
    /// A program outside pacman, in a folder on `$PATH`, `~/.local/bin` or `~/.cargo/bin`.
    Program(PathBuf),
    AppImage(PathBuf),
    /// A desktop entry outside pacman and Flatpak whose program exists.
    Desktop(PathBuf),
    /// A folder on `$PATH`, which the folder it is in holds: `~/.nvm` holds
    /// `~/.nvm/versions/node/v22/bin`.
    OnPath(PathBuf),
    /// Part of the desktop, or of vole itself, whatever is installed: `~/.config/mimeapps.list`.
    Shared,
    /// The user's Known Owners file says so, naming the owner.
    Known(String),
}

/// Folders not named (only) after their owner, and what owns them: whichever of the listed names
/// is installed, as a package, program, Flatpak, AppImage or desktop entry. A pattern matches a
/// name the folder goes by as a whole, by its start (`p10k*` for `p10k-dump-me.zsh`), or by its
/// end (`*-nodejs`). Folders of the user's own things belong in their Known Owners file.
const KNOWN_NAMES: &[(&str, &[&str])] = &[
    // KDE Plasma and Frameworks.
    ("kdeglobals", &["kconfig", "plasma-workspace"]),
    ("kdedefaults", &["kconfig", "plasma-workspace"]),
    ("kglobalshortcuts", &["kglobalacceld", "plasma-workspace"]),
    (
        "kcm*",
        &["plasma-desktop", "plasma-workspace", "systemsettings"],
    ),
    ("kioslave", &["kio"]),
    ("kcookiejar", &["kio"]),
    ("kuriikwsfilter", &["kio"]),
    ("klipper", &["plasma-workspace"]),
    ("kxkb", &["plasma-desktop"]),
    ("ktimezoned", &["plasma-workspace"]),
    ("powermanagementprofiles", &["powerdevil"]),
    ("systemmonitor", &["plasma-systemmonitor", "ksystemstats"]),
    ("ksplash", &["plasma-workspace"]),
    ("session", &["plasma-workspace"]),
    ("plasma*", &["plasma-workspace"]),
    ("ksycoca*", &["kservice"]),
    ("user-places*", &["kio"]),
    ("kicker", &["plasma-desktop", "plasma-workspace"]),
    ("org.kde.unitconversion", &["kunitconversion"]),
    // Qt and GTK.
    ("qtproject", &["qt6-base", "qt5-base"]),
    ("trolltech", &["qt6-base", "qt5-base"]),
    ("qtshadercache*", &["qt6-base", "qt5-base"]),
    ("gtkrc*", &["gtk2", "gtk3", "gtk4", "kde-gtk-config"]),
    ("event-sound-cache*", &["libcanberra"]),
    ("pulse*", &["libpulse", "pulseaudio", "pipewire-pulse"]),
    // Shells.
    ("histfile", &["zsh"]),
    ("zcompdump*", &["zsh"]),
    ("p10k*", &["zsh"]),
    ("gitstatus", &["zsh"]),
    ("oh-my-zsh", &["zsh"]),
    ("inputrc", &["readline", "bash"]),
    // Editors, and the plugins that keep their own folders.
    ("vscode*", &["code", "codium"]),
    (
        "copilot",
        &["code", "codium", "jetbrains-toolbox", "nvim", "vim", "gh"],
    ),
    (
        "github-copilot",
        &["code", "codium", "jetbrains-toolbox", "nvim", "vim"],
    ),
    ("ghcp-appmod*", &["code", "codium", "jetbrains-toolbox"]),
    (
        "wakatime*",
        &[
            "code",
            "codium",
            "jetbrains-toolbox",
            "nvim",
            "vim",
            "emacs",
        ],
    ),
    // JetBrains IDEs, and the JDKs and debugging symbols they download.
    ("jdks", &["jetbrains-toolbox", "idea", "android-studio"]),
    ("jdk", &["java", "jetbrains-toolbox", "android-studio"]),
    ("junie", &["jetbrains-toolbox"]),
    ("refsrcsymbols", &["rider", "jetbrains-toolbox"]),
    ("symbolsourcesymbols", &["rider", "jetbrains-toolbox"]),
    ("symbols", &["rider", "jetbrains-toolbox"]),
    ("kotlin", &["kotlin", "jetbrains-toolbox", "idea", "gradle"]),
    // Flatpak keeps its apps' data in ~/.var.
    ("var", &["flatpak"]),
    // Toolchains.
    ("java", &["java"]),
    ("gradle", &["gradle", "java"]),
    ("m2", &["mvn", "java"]),
    ("net", &["dotnet", "mono"]),
    ("nuget", &["dotnet", "mono", "nuget"]),
    ("isolatedstorage", &["dotnet", "mono"]),
    ("android", &["android-studio", "adb", "android-tools"]),
    ("emulator*", &["android-studio", "emulator"]),
    ("kube", &["kubectl", "k9s", "minikube", "helm"]),
    ("configstore", &["node", "npm"]),
    ("storybook", &["node", "npm"]),
    ("*-nodejs", &["node", "npm"]),
    ("kitware", &["cmake"]),
    // Apps whose folders go by their maker's name.
    (
        "google",
        &[
            "google-chrome",
            "chromium",
            "android-studio",
            "google-earth-pro",
        ],
    ),
    ("microsoft", &["microsoft-edge", "code", "teams-for-linux"]),
    ("mozilla", &["firefox", "thunderbird", "librewolf"]),
    ("meltytech", &["shotcut"]),
    (
        "raspberry pi",
        &["rpi-imager", "com.raspberrypi.rpi-imager"],
    ),
];

/// Folders that are part of the desktop, or of vole itself: owned whatever is installed. Matched
/// like [`KNOWN_NAMES`].
const SHARED: &[&str] = &[
    "applications",
    "autostart",
    "dbus-1",
    "desktop-directories",
    "environment.d",
    "fonts",
    "icons",
    "menus",
    "mime",
    "mimeapps",
    "recently-used",
    "sounds",
    "systemd",
    "themes",
    "trash",
    "user-dirs",
    "vole",
];

/// Whether `pattern` (see [`KNOWN_NAMES`]) matches one of `keys`.
fn matches(pattern: &str, keys: &[String]) -> bool {
    if let Some(end) = pattern.strip_prefix('*') {
        keys.iter().any(|key| key.ends_with(end))
    } else if let Some(start) = pattern.strip_suffix('*') {
        keys.iter().any(|key| key.starts_with(start))
    } else {
        keys.iter().any(|key| key == pattern)
    }
}

/// A folder's name may start with its owner's name (`katemetainfos`, `kwinsession`), but only
/// names at least this long count, so `kio` doesn't claim `kiosk-browser`.
const MIN_PREFIX: usize = 4;

/// An owner's name may also start with a folder's whole name (`BurpSuiteCommunity` for
/// `~/.BurpSuite`), when the folder's name is at least this long.
const MIN_NAME_START: usize = 6;

/// Names too common to tell an owner by.
const GENERIC: &[&str] = &[
    "app", "com", "config", "data", "default", "dev", "github", "gitlab", "io", "lib", "local",
    "net", "new", "old", "org", "share", "state", "the", "tmp", "user",
];

/// The parts of reverse-DNS names that name a vendor, not a program: `org.kde.kdenlive` is
/// kdenlive's, not `kde`'s.
const VENDORS: &[&str] = &["flathub", "freedesktop", "gnome", "kde", "sourceforge"];

/// The folders whose entries are named after what installs them: `/usr/share/fish` is fish's.
/// Deeper down, names say less: ncurses installs `/usr/share/terminfo/a/alacritty`.
const INSTALL_DIRS: &[&str] = &[
    "/usr/share/",
    "/usr/lib/",
    "/usr/lib32/",
    "/usr/libexec/",
    "/etc/",
    "/etc/xdg/",
    "/opt/",
];

/// Extensions a config file's name may end with.
const EXTENSIONS: &[&str] = &[
    ".bak",
    ".cache",
    ".cfg",
    ".conf",
    ".dat",
    ".db",
    ".dirs",
    ".ini",
    ".json",
    ".jsonc",
    ".kcache",
    ".list",
    ".locale",
    ".lock",
    ".log",
    ".old",
    ".pid",
    ".properties",
    ".sqlite",
    ".state",
    ".tdb",
    ".tmp",
    ".toml",
    ".txt",
    ".xbel",
    ".xml",
    ".yaml",
    ".yml",
    ".zsh",
    ".zwc",
];

/// What config files are often called after their program's name: `dolphinrc`,
/// `dolphinstaterc`, `python_history`, `lesshst`, `viminfo`, `wget-hsts`, `gitconfig`, `zshenv`.
const SUFFIXES: &[&str] = &[
    "staterc", "rc", "_history", "-history", "history", "-hsts", "_hsts", "hsts", "hst", "info",
    "-config", "_config", "config", "state", "env",
];

/// What package names often add to their program's name.
const PACKAGE_SUFFIXES: &[&str] = &[
    "-appimage",
    "-beta",
    "-bin",
    "-browser",
    "-desktop",
    "-git",
    "-launcher",
    "-nightly",
    "-stable",
];

fn add_key(keys: &mut Vec<String>, key: &str) {
    if key.len() >= 2 && !GENERIC.contains(&key) && !keys.iter().any(|k| k == key) {
        keys.push(key.to_string());
    }
}

/// The names a file or folder called `name` in the home directory goes by as a whole, lowercase:
/// `.python_history` → `python_history`, `python`; `kded6rc` → …, `kded`.
pub fn primary_keys(name: &str) -> Vec<String> {
    let mut keys = Vec::new();
    let lower = name.to_lowercase();
    let full = lower.strip_prefix('.').unwrap_or(&lower);
    add_key(&mut keys, full);
    let stem = strip_extensions(full);
    add_key(&mut keys, stem);
    let base = strip_suffix(stem);
    add_key(&mut keys, base);
    add_key(&mut keys, strip_version(base));
    if let Some(rest) = base.strip_prefix("lib") {
        add_key(&mut keys, rest);
    }
    keys
}

/// The names a file or folder called `name` in the home directory may go by, lowercase and most
/// specific first: [`primary_keys`], then parts of the name: `Code - OSS` → `code - oss`, `code`;
/// `org.kde.dolphin` → …, `dolphin`.
pub fn name_keys(name: &str) -> Vec<String> {
    let mut keys = primary_keys(name);
    let mut add = |key: &str| add_key(&mut keys, key);
    let lower = name.to_lowercase();
    let full = lower.strip_prefix('.').unwrap_or(&lower);
    let stem = strip_extensions(full);
    // The first word: obs-studio → obs, code - oss → code, plasma_notes → plasma.
    let first = stem
        .split(['-', '_', '.', ' ', '+'])
        .find(|w| !w.is_empty());
    if let Some(first) = first.filter(|w| w.len() >= 3) {
        add(first);
        add(strip_suffix(first));
        add(strip_version(first));
    }
    // Reverse-DNS names: org.kde.dolphin, UserFeedback.org.kde.dolphin → dolphin.
    if stem.matches('.').count() >= 2 {
        for segment in stem
            .split('.')
            .filter(|s| s.len() >= 3 && !VENDORS.contains(s))
        {
            add(segment);
            if let Some(word) = segment.split(['-', '_']).next() {
                add(word);
            }
        }
    }
    keys
}

/// The names a package may be found by: `slack-desktop` → `slack-desktop`, `slack`;
/// `microsoft-edge-stable-bin` → …, `microsoft-edge`; `electron37` → `electron37`, `electron`.
fn package_keys(name: &str) -> Vec<String> {
    let mut keys = vec![name.to_lowercase()];
    let mut current = keys[0].clone();
    while let Some(stripped) = PACKAGE_SUFFIXES
        .iter()
        .find_map(|suffix| current.strip_suffix(suffix))
        .filter(|rest| rest.len() >= 2)
    {
        current = stripped.to_string();
        keys.push(current.clone());
    }
    let unversioned = strip_version(&current);
    if unversioned != current {
        keys.push(unversioned.to_string());
    }
    keys
}

fn strip_extensions(name: &str) -> &str {
    let mut name = name;
    while let Some(stripped) = EXTENSIONS
        .iter()
        .find_map(|ext| name.strip_suffix(ext))
        .filter(|rest| !rest.is_empty())
    {
        name = stripped;
    }
    name
}

fn strip_suffix(name: &str) -> &str {
    SUFFIXES
        .iter()
        .find_map(|suffix| name.strip_suffix(suffix))
        .filter(|rest| rest.len() >= 2)
        .unwrap_or(name)
}

/// `kded6` → `kded`, `gegl-0.4` → `gegl`; unchanged when too little would be left.
fn strip_version(name: &str) -> &str {
    let stripped = name.trim_end_matches(|c: char| c.is_ascii_digit() || "._-".contains(c));
    if stripped.len() >= 3 { stripped } else { name }
}

/// The names a Flatpak app id goes by: `com.spotify.Client` → `com.spotify.client`, `spotify`,
/// `client`.
fn flatpak_keys(id: &str) -> Vec<String> {
    let lower = id.to_lowercase();
    let mut keys = Vec::new();
    add_key(&mut keys, &lower);
    for segment in lower
        .split('.')
        .filter(|s| s.len() >= 3 && !VENDORS.contains(s))
    {
        add_key(&mut keys, segment);
    }
    keys
}

/// The names an AppImage goes by: `Obsidian-1.4.16.AppImage` → `obsidian-1.4.16`, `obsidian`.
fn appimage_keys(path: &Path) -> Vec<String> {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let lower = name.to_lowercase();
    let stem = lower.strip_suffix(".appimage").unwrap_or(&lower);
    let mut keys = Vec::new();
    add_key(&mut keys, stem);
    add_key(&mut keys, strip_version(stem));
    if let Some(first) = stem.split(['-', '_', '.', ' ']).find(|w| w.len() >= 3) {
        add_key(&mut keys, first);
    }
    keys
}

/// The names a program goes by: its file name, and without a version (`python3.13` → `python`).
fn program_keys(path: &Path) -> Vec<String> {
    let name = path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_lowercase();
    let mut keys = Vec::new();
    add_key(&mut keys, &name);
    add_key(&mut keys, strip_version(&name));
    keys
}

/// The owners vole knows about, by the names they go by.
#[derive(Default)]
pub struct Owners {
    /// Owners by a name a folder may go by exactly.
    exact: HashMap<String, Vec<Owner>>,
    /// Owners by the names of programs and packages, which a folder's name may also start with,
    /// or be close to.
    named: HashMap<String, Vec<Owner>>,
    /// Packages by the first word of their name, `jetbrains` for `jetbrains-toolbox`: a folder
    /// called just that is theirs, but `gnome-boxes` isn't `gnome-calculator`'s.
    first_words: HashMap<String, Vec<Owner>>,
    /// The folders on `$PATH`, resolved.
    path_dirs: Vec<PathBuf>,
    /// The programs pacman installed, so they aren't counted twice.
    pacman_programs: HashSet<PathBuf>,
    /// The desktop entries pacman installed, and their packages.
    pacman_desktop: HashMap<PathBuf, Owner>,
    /// The Known Owners file's entries, by resolved path.
    known: HashMap<PathBuf, String>,
}

impl Owners {
    /// From pacman: the names of the `installed` packages, the programs they install in
    /// `/usr/bin`, and the folders and files they install whose name is in `wanted`, the names
    /// the files and folders looked at go by. `files` is the output of `pacman -Ql`.
    pub fn from_pacman(installed: &[Info], files: &str, wanted: &HashSet<String>) -> Self {
        let mut owners = Self::default();
        for info in installed {
            let owner = Owner::Package(info.name.clone());
            for key in package_keys(&info.name) {
                owners.add_named(key, &owner);
            }
            let first = info
                .name
                .split('-')
                .next()
                .unwrap_or_default()
                .to_lowercase();
            if first.len() >= 3 && first != info.name && !GENERIC.contains(&first.as_str()) {
                add_owner(owners.first_words.entry(first).or_default(), &owner);
            }
        }
        for line in files.lines() {
            let Some((package, path)) = line.split_once(' ') else {
                continue;
            };
            let owner = Owner::Package(package.to_string());
            let is_program = Path::new(path)
                .parent()
                .is_some_and(|dir| dir.ends_with("bin") || dir.ends_with("sbin"));
            if is_program {
                owners.pacman_programs.insert(PathBuf::from(path));
            } else if path.ends_with(".desktop") {
                owners
                    .pacman_desktop
                    .insert(PathBuf::from(path), owner.clone());
            }
            if let Some(program) = path.strip_prefix("/usr/bin/")
                && !program.is_empty()
                && !program.contains('/')
            {
                for key in program_keys(Path::new(program)) {
                    owners.add_named(key, &owner);
                }
            }
            // Installed folders and files naming the folder: /usr/share/fish for ~/.config/fish.
            for dir in INSTALL_DIRS {
                let Some(rest) = path.strip_prefix(dir) else {
                    continue;
                };
                let name = rest.split('/').next().unwrap_or_default();
                if wanted.contains(name) {
                    owners.add(name.to_string(), &owner);
                } else if name.bytes().any(|b| b.is_ascii_uppercase()) {
                    let lower = name.to_lowercase();
                    if wanted.contains(&lower) {
                        owners.add(lower, &owner);
                    }
                }
            }
        }
        owners
    }

    /// What is installed outside pacman: programs on `$PATH` (the folders on it hold the
    /// folders they are in), desktop entries, Flatpaks and AppImages.
    pub fn add_outside_pacman(&mut self, outside: &OutsidePacman) {
        self.add_programs(&outside.path_dirs, &outside.programs);
        self.add_desktop_entries(&outside.desktop_entries);
        self.add_flatpaks(&outside.flatpaks);
        self.add_appimages(&outside.appimages);
    }

    /// The programs in the folders on `$PATH` that pacman didn't install.
    fn add_programs(&mut self, path_dirs: &[PathBuf], programs: &[PathBuf]) {
        self.path_dirs.extend_from_slice(path_dirs);
        for program in programs {
            if self.pacman_programs.contains(program) {
                continue;
            }
            let owner = Owner::Program(program.clone());
            for key in program_keys(program) {
                self.add_named(key, &owner);
            }
        }
    }

    /// Desktop entries: by their id (its last part for reverse-DNS ids), window class and
    /// program. One pacman didn't install, nor Flatpak, counts only while its program exists.
    fn add_desktop_entries(&mut self, entries: &[detect::DesktopEntry]) {
        for entry in entries.iter().filter(|entry| !entry.hidden) {
            let owner = if let Some(package) = self.pacman_desktop.get(&entry.path) {
                package.clone()
            } else if entry.path.to_string_lossy().contains("/flatpak/exports/") {
                Owner::Flatpak(entry.id.clone())
            } else if entry
                .program
                .as_deref()
                .is_some_and(|program| detect::program_exists(program, &self.path_dirs))
            {
                Owner::Desktop(entry.path.clone())
            } else {
                continue;
            };
            let mut keys = Vec::new();
            let id = entry.id.to_lowercase();
            add_key(&mut keys, &id);
            if id.matches('.').count() >= 2
                && let Some(last) = id.rsplit('.').next()
            {
                add_key(&mut keys, last);
            }
            if let Some(class) = &entry.wm_class {
                add_key(&mut keys, &class.to_lowercase());
            }
            if let Some(program) = &entry.program {
                keys.extend(program_keys(Path::new(program)));
            }
            for key in keys {
                self.add_named(key, &owner);
            }
        }
    }

    /// Flatpak apps, by their id's parts.
    fn add_flatpaks(&mut self, ids: &[String]) {
        for id in ids {
            let owner = Owner::Flatpak(id.clone());
            for key in flatpak_keys(id) {
                self.add_named(key, &owner);
            }
        }
    }

    /// AppImages, by their names without the version.
    fn add_appimages(&mut self, appimages: &[PathBuf]) {
        for appimage in appimages {
            let owner = Owner::AppImage(appimage.clone());
            for key in appimage_keys(appimage) {
                self.add_named(key, &owner);
            }
        }
    }

    /// The Known Owners file's entries: resolved paths, and what owns each.
    pub fn add_known(&mut self, known: impl IntoIterator<Item = (PathBuf, String)>) {
        self.known.extend(known);
    }

    fn add(&mut self, key: String, owner: &Owner) {
        add_owner(self.exact.entry(key).or_default(), owner);
    }

    fn add_named(&mut self, key: String, owner: &Owner) {
        add_owner(self.named.entry(key.clone()).or_default(), owner);
        self.add(key, owner);
    }

    /// Who owns the file or folder at `real` (resolved), called `name`.
    pub fn of(&self, real: &Path, name: &str) -> Vec<Owner> {
        let mut found: Vec<Owner> = Vec::new();
        for dir in self.path_dirs.iter().filter(|dir| dir.starts_with(real)) {
            add_owner(&mut found, &Owner::OnPath(dir.clone()));
        }
        let mut add = |owners: Option<&Vec<Owner>>| {
            for owner in owners.into_iter().flatten() {
                add_owner(&mut found, owner);
            }
        };
        for key in name_keys(name) {
            add(self.exact.get(&key));
            // Starting with a program's or package's name: katemetainfos → kate.
            for end in (MIN_PREFIX..key.len()).filter(|&end| key.is_char_boundary(end)) {
                add(self.named.get(&key[..end]));
            }
        }
        for key in primary_keys(name) {
            add(self.first_words.get(&key));
            // The start of a program's or package's name: .BurpSuite for BurpSuiteCommunity.
            if key.len() >= MIN_NAME_START {
                for (owner_key, owners) in &self.named {
                    if owner_key.starts_with(key.as_str()) {
                        add(Some(owners));
                    }
                }
            }
        }
        // The whole name too, even when too common to go by alone: ~/.net.
        let mut keys = name_keys(name);
        let lower = name.to_lowercase();
        keys.push(lower.strip_prefix('.').unwrap_or(&lower).to_string());
        for (pattern, owners) in KNOWN_NAMES {
            if matches(pattern, &keys) {
                for owner in owners.iter().copied() {
                    add(self.exact.get(owner));
                }
            }
        }
        if SHARED.iter().any(|pattern| matches(pattern, &keys)) {
            add_owner(&mut found, &Owner::Shared);
        }
        // A Known Owners entry keeps what holds it too, like the Protected List: an entry for
        // ~/.local/share/Steam/steamapps keeps ~/.local/share/Steam.
        for (path, owner) in &self.known {
            if path.starts_with(real) || real.starts_with(path) {
                add_owner(&mut found, &Owner::Known(owner.clone()));
            }
        }
        found
    }

    /// A program or package whose name contains a name `name` goes by, or is contained in it,
    /// which makes `name` less surely an orphan: `text-editor` and `gnome-text-editor`.
    pub fn close_to(&self, name: &str) -> Option<String> {
        let keys = name_keys(name);
        let close = |owner_key: &str| {
            keys.iter().filter(|key| key.len() >= 5).any(|key| {
                owner_key.len() >= 5
                    && (owner_key.contains(key.as_str()) || key.contains(owner_key))
            })
        };
        let mut names: Vec<&String> = self.named.keys().filter(|key| close(key)).collect();
        names.sort();
        names.first().map(|name| name.to_string())
    }
}

fn add_owner(owners: &mut Vec<Owner>, owner: &Owner) {
    if !owners.contains(owner) {
        owners.push(owner.clone());
    }
}

/// The names the files and folders called `names` go by, for [`Owners::from_pacman`].
pub fn wanted<'a>(names: impl IntoIterator<Item = &'a str>) -> HashSet<String> {
    names.into_iter().flat_map(name_keys).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testdir::TestDir;

    fn info(name: &str) -> Info {
        Info {
            name: name.into(),
            ..Info::default()
        }
    }

    fn package(name: &str) -> Owner {
        Owner::Package(name.into())
    }

    /// The owners of a file or folder in `~/.config` called `name`.
    fn of(owners: &Owners, name: &str) -> Vec<Owner> {
        owners.of(&Path::new("/home/me/.config").join(name), name)
    }

    #[test]
    fn names_go_by_their_programs_name() {
        let cases: &[(&str, &[&str])] = &[
            ("dolphinstaterc", &["dolphin"]),
            ("kwinrc", &["kwin"]),
            (".python_history", &["python"]),
            (".lesshst", &["less"]),
            (".viminfo", &["vim"]),
            (".wget-hsts", &["wget"]),
            (".gitconfig", &["git"]),
            (".zshenv", &["zsh"]),
            ("starship.toml", &["starship"]),
            ("btop.log", &["btop"]),
            ("Code - OSS", &["code - oss", "code"]),
            ("obs-studio", &["obs-studio", "obs"]),
            ("gegl-0.4", &["gegl"]),
            ("kded6", &["kded"]),
            ("libkunitconversion", &["kunitconversion"]),
            ("UserFeedback.org.kde.dolphin", &["dolphin"]),
            ("org.localsend.localsend_app", &["localsend"]),
            (".zshrc.pre-oh-my-zsh", &["zsh"]),
        ];
        for (name, expected) in cases {
            let keys = name_keys(name);
            for key in *expected {
                assert!(keys.iter().any(|k| k == key), "{name}: {keys:?}");
            }
        }
        assert!(!name_keys("org.kde.dolphin").iter().any(|k| k == "org"));
    }

    #[test]
    fn packages_go_by_shorter_names() {
        assert_eq!(package_keys("slack-desktop"), ["slack-desktop", "slack"]);
        assert_eq!(
            package_keys("microsoft-edge-stable-bin"),
            [
                "microsoft-edge-stable-bin",
                "microsoft-edge-stable",
                "microsoft-edge"
            ]
        );
        assert_eq!(package_keys("electron37"), ["electron37", "electron"]);
        assert_eq!(package_keys("bin"), ["bin"]);
    }

    #[test]
    fn finds_owners_by_package_program_and_installed_path() {
        let installed = [info("neovim"), info("fish"), info("kate"), info("htop")];
        let files = "\
neovim /usr/bin/nvim
neovim /usr/share/nvim/
fish /usr/share/fish/
fish /usr/share/fish/config.fish
kate /usr/bin/kate
htop /usr/bin/htop
";
        let names = ["nvim", "fish", "katemetainfos", "htop", "gimp"];
        let owners = Owners::from_pacman(&installed, files, &wanted(names));
        assert_eq!(of(&owners, "nvim"), [package("neovim")]);
        assert_eq!(of(&owners, "fish"), [package("fish")]);
        assert_eq!(of(&owners, "katemetainfos"), [package("kate")]);
        assert_eq!(of(&owners, "htop"), [package("htop")]);
        assert_eq!(of(&owners, ".htoprc"), [package("htop")]);
        assert!(of(&owners, "gimp").is_empty());
        // Not every installed path counts: only those that were wanted.
        assert!(of(&owners, "share").is_empty());
    }

    #[test]
    fn only_folders_named_right_under_install_folders_count() {
        let files = "\
ncurses /usr/share/terminfo/a/alacritty
hicolor-icon-theme /usr/share/icons/hicolor/48x48/stock/code/
filesystem /etc/profile
picom /etc/xdg/picom/
";
        let names = ["alacritty", "Code", ".profile", "picom"];
        let owners = Owners::from_pacman(&[], files, &wanted(names));
        assert!(of(&owners, "alacritty").is_empty());
        assert!(of(&owners, "Code").is_empty());
        assert_eq!(of(&owners, ".profile"), [package("filesystem")]);
        assert_eq!(of(&owners, "picom"), [package("picom")]);
    }

    #[test]
    fn vendors_name_no_owner() {
        let installed = [info("dolphin")];
        let files = "kio /usr/share/kde/\n";
        let names = ["org.kde.kdenlive", "org.kde.dolphin"];
        let mut owners = Owners::from_pacman(&installed, files, &wanted(names));
        owners.add_outside_pacman(&OutsidePacman {
            flatpaks: vec!["org.kde.krita".into()],
            ..outside()
        });
        assert!(of(&owners, "org.kde.kdenlive").is_empty());
        assert_eq!(of(&owners, "org.kde.dolphin"), [package("dolphin")]);
    }

    #[test]
    fn a_folder_may_be_the_start_of_its_owners_name() {
        let installed = [info("burpsuitecommunity"), info("steamcmd")];
        let owners = Owners::from_pacman(&installed, "", &HashSet::new());
        assert_eq!(
            owners.of(Path::new("/home/me/.BurpSuite"), ".BurpSuite"),
            [package("burpsuitecommunity")]
        );
        // A short name says too little: steamcmd is not Steam.
        assert!(of(&owners, "steam").is_empty());
    }

    #[test]
    fn short_names_are_not_prefixes() {
        let installed = [info("kio")];
        let owners = Owners::from_pacman(&installed, "", &HashSet::new());
        assert!(of(&owners, "kiosk-browser").is_empty());
        assert_eq!(of(&owners, "kiorc"), [package("kio")]);
    }

    #[test]
    fn names_close_to_a_package() {
        let installed = [info("gnome-text-editor")];
        let owners = Owners::from_pacman(&installed, "", &HashSet::new());
        assert!(of(&owners, "text-editor").is_empty());
        assert_eq!(
            owners.close_to("text-editor").as_deref(),
            Some("gnome-text-editor")
        );
        assert_eq!(owners.close_to("gimp"), None);
    }

    #[test]
    fn package_first_words_name_only_whole_folders() {
        let installed = [info("jetbrains-toolbox"), info("gnome-calculator")];
        let owners = Owners::from_pacman(&installed, "", &HashSet::new());
        assert_eq!(of(&owners, "JetBrains"), [package("jetbrains-toolbox")]);
        assert_eq!(of(&owners, "gnome"), [package("gnome-calculator")]);
        assert!(of(&owners, "gnome-boxes").is_empty());
    }

    fn outside() -> OutsidePacman {
        OutsidePacman::default()
    }

    #[test]
    fn finds_programs_outside_pacman() {
        let files = "pacman /usr/bin/pacman\n";
        let mut owners = Owners::from_pacman(&[info("pacman")], files, &HashSet::new());
        let claude = PathBuf::from("/home/me/.local/bin/claude");
        owners.add_outside_pacman(&OutsidePacman {
            path_dirs: vec!["/home/me/.local/bin".into(), "/usr/bin".into()],
            programs: vec![
                claude.clone(),
                "/usr/bin/pacman".into(),
                "/usr/bin/rclone".into(),
            ],
            ..outside()
        });
        assert_eq!(of(&owners, "claude"), [Owner::Program(claude)]);
        assert_eq!(
            of(&owners, "rclone"),
            [Owner::Program("/usr/bin/rclone".into())]
        );
        // pacman's own programs stay its package's.
        assert_eq!(of(&owners, "pacman"), [package("pacman")]);
    }

    #[test]
    fn a_folder_holding_a_path_folder_is_in_use() {
        let mut owners = Owners::default();
        let node = PathBuf::from("/home/me/.nvm/versions/node/v22/bin");
        owners.add_outside_pacman(&OutsidePacman {
            path_dirs: vec![node.clone()],
            ..outside()
        });
        assert_eq!(
            owners.of(Path::new("/home/me/.nvm"), ".nvm"),
            [Owner::OnPath(node)]
        );
        assert!(owners.of(Path::new("/home/me/.nvmrc"), ".nvmrc").is_empty());
    }

    #[test]
    fn finds_desktop_entries_whose_program_exists() {
        let entry = |path: &str, wm_class: Option<&str>, program: &str| detect::DesktopEntry {
            path: path.into(),
            id: Path::new(path)
                .file_stem()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
            wm_class: wm_class.map(String::from),
            program: Some(program.into()),
            hidden: false,
        };
        let files = "visual-studio-code-bin /usr/share/applications/code.desktop\n";
        let mut owners =
            Owners::from_pacman(&[info("visual-studio-code-bin")], files, &HashSet::new());
        let existing = TestDir::new();
        let program = existing.file("app/run", 1);
        let mine = format!("{}/mine.desktop", existing.path().display());
        owners.add_outside_pacman(&OutsidePacman {
            desktop_entries: vec![
                entry("/usr/share/applications/code.desktop", Some("Code"), "code"),
                entry(&mine, Some("MyApp"), &program.display().to_string()),
                entry(
                    "/home/me/.local/share/applications/gone.desktop",
                    None,
                    "/opt/gone/gone",
                ),
                entry(
                    "/var/lib/flatpak/exports/share/applications/org.kde.krita.desktop",
                    None,
                    "/usr/bin/flatpak",
                ),
            ],
            ..outside()
        });
        assert_eq!(of(&owners, "Code"), [package("visual-studio-code-bin")]);
        assert_eq!(of(&owners, "MyApp"), [Owner::Desktop(mine.into())]);
        assert!(of(&owners, "gone").is_empty());
        assert_eq!(
            of(&owners, "krita"),
            [Owner::Flatpak("org.kde.krita".into())]
        );
    }

    #[test]
    fn finds_flatpaks_and_appimages() {
        let mut owners = Owners::default();
        let obsidian = PathBuf::from("/home/me/Applications/Obsidian-1.4.16.AppImage");
        owners.add_outside_pacman(&OutsidePacman {
            flatpaks: vec!["com.spotify.Client".into()],
            appimages: vec![obsidian.clone()],
            ..outside()
        });
        assert_eq!(
            of(&owners, "spotify"),
            [Owner::Flatpak("com.spotify.Client".into())]
        );
        assert_eq!(of(&owners, "obsidian"), [Owner::AppImage(obsidian)]);
        // A generic part of an id isn't a name to own folders by.
        assert!(of(&owners, "com").is_empty());
    }

    #[test]
    fn some_folders_go_by_other_names() {
        let installed = [info("kconfig"), info("zsh"), info("cmake")];
        let mut owners = Owners::from_pacman(&installed, "", &HashSet::new());
        let node = PathBuf::from("/home/me/.nvm/versions/node/v22/bin/node");
        owners.add_outside_pacman(&OutsidePacman {
            programs: vec![node.clone()],
            ..outside()
        });
        assert_eq!(of(&owners, "kdeglobals"), [package("kconfig")]);
        let kio = Owners::from_pacman(&[info("kio")], "", &HashSet::new());
        assert_eq!(of(&kio, "kioslaverc"), [package("kio")]);
        assert_eq!(of(&owners, "Kitware"), [package("cmake")]);
        for name in [
            ".zcompdump-host-5.9.zwc",
            "p10k-dump-me.zsh.tmp.131579",
            ".histfile",
        ] {
            assert_eq!(of(&owners, name), [package("zsh")], "{name}");
        }
        assert_eq!(of(&owners, "nextjs-nodejs"), [Owner::Program(node)]);
        // Only while what owns them is installed.
        assert!(of(&owners, "qtshadercache-x86_64-little_endian-lp64").is_empty());
        // Flatpak's apps keep their data in ~/.var.
        let owners_with_flatpak = Owners::from_pacman(&[info("flatpak")], "", &HashSet::new());
        assert_eq!(
            owners_with_flatpak.of(Path::new("/home/me/.var"), ".var"),
            [package("flatpak")]
        );
        // A name too common to go by alone still counts as a whole: ~/.net.
        let owners = Owners::from_pacman(&[info("dotnet")], "", &HashSet::new());
        assert_eq!(
            owners.of(Path::new("/home/me/.net"), ".net"),
            [package("dotnet")]
        );
    }

    #[test]
    fn desktop_folders_and_vole_are_shared() {
        let owners = Owners::default();
        for name in [
            "mimeapps.list",
            "user-dirs.dirs",
            "Trash",
            "vole",
            "recently-used.xbel",
        ] {
            assert_eq!(of(&owners, name), [Owner::Shared], "{name}");
        }
    }

    #[test]
    fn known_owners_own_their_folders() {
        let mut owners = Owners::default();
        let tool = PathBuf::from("/home/me/.config/my-tool");
        owners.add_known([(tool.clone(), "my script".to_string())]);
        assert_eq!(
            owners.of(&tool, "my-tool"),
            [Owner::Known("my script".into())]
        );
        assert!(of(&owners, "other-tool").is_empty());
        // What holds an entry, or is inside one, is kept too.
        let library = PathBuf::from("/home/me/.local/share/Steam/steamapps");
        let mut owners = Owners::default();
        owners.add_known([(library, "my game library".to_string())]);
        assert_eq!(
            owners.of(Path::new("/home/me/.local/share/Steam"), "Steam"),
            [Owner::Known("my game library".into())]
        );
        let mut owners = Owners::default();
        owners.add_known([(PathBuf::from("/home/me/.config"), "me".to_string())]);
        assert_eq!(of(&owners, "anything").len(), 1);
    }
}
