//! Owners: what installed thing accounts for a file or folder in the home directory.
//!
//! A folder belongs to whatever it is named after. Its name is turned into the names it may go
//! by ([`name_keys`]: `dolphinstaterc` → `dolphin`), and those are looked up among the names of
//! what is installed. Matching generously is the safe side: a folder wrongly thought owned is only
//! not proposed, while one wrongly thought orphaned would be.

use std::collections::{HashMap, HashSet};
use std::fmt;

use super::pacman::Info;

/// Something installed that accounts for a config or data folder.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Owner {
    /// A pacman package.
    Package(String),
}

impl fmt::Display for Owner {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Owner::Package(name) => f.write_str(name),
        }
    }
}

/// A folder's name may start with its owner's name (`katemetainfos`, `kwinsession`), but only
/// names at least this long count, so `kio` doesn't claim `kiosk-browser`.
const MIN_PREFIX: usize = 4;

/// Names too common to tell an owner by.
const GENERIC: &[&str] = &[
    "app", "com", "config", "data", "default", "dev", "github", "gitlab", "io", "lib", "local",
    "net", "new", "old", "org", "share", "state", "the", "tmp", "user",
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

/// The names a file or folder called `name` in the home directory may go by, lowercase and most
/// specific first: `Code - OSS` → `code - oss`, `code`; `dolphinstaterc` → `dolphinstaterc`,
/// `dolphin`; `org.kde.dolphin` → …, `dolphin`.
pub fn name_keys(name: &str) -> Vec<String> {
    let mut keys = Vec::new();
    let mut add = |key: &str| {
        if key.len() >= 2 && !GENERIC.contains(&key) && !keys.iter().any(|k| k == key) {
            keys.push(key.to_string());
        }
    };
    let lower = name.to_lowercase();
    let full = lower.strip_prefix('.').unwrap_or(&lower);
    add(full);
    let stem = strip_extensions(full);
    add(stem);
    let base = strip_suffix(stem);
    add(base);
    add(strip_version(base));
    if let Some(rest) = base.strip_prefix("lib") {
        add(rest);
    }
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
        for segment in stem.split('.').filter(|s| s.len() >= 3) {
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

/// The owners vole knows about, by the names they go by.
#[derive(Default)]
pub struct Owners {
    /// Owners by a name a folder may go by exactly.
    exact: HashMap<String, Vec<Owner>>,
    /// Owners by the names of programs and packages, which a folder's name may also start with,
    /// or be close to.
    named: HashMap<String, Vec<Owner>>,
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
        }
        for line in files.lines() {
            let Some((package, path)) = line.split_once(' ') else {
                continue;
            };
            let owner = Owner::Package(package.to_string());
            if let Some(program) = path.strip_prefix("/usr/bin/")
                && !program.is_empty()
                && !program.contains('/')
            {
                let program = program.to_lowercase();
                let unversioned = strip_version(&program).to_string();
                owners.add_named(unversioned, &owner);
                owners.add_named(program, &owner);
            }
            // Installed paths naming the folder: /usr/share/fish for ~/.config/fish.
            for component in path.split('/').filter(|c| !c.is_empty()) {
                if wanted.contains(component) {
                    owners.add(component.to_string(), &owner);
                } else if component.bytes().any(|b| b.is_ascii_uppercase()) {
                    let lower = component.to_lowercase();
                    if wanted.contains(&lower) {
                        owners.add(lower, &owner);
                    }
                }
            }
        }
        owners
    }

    fn add(&mut self, key: String, owner: &Owner) {
        let owners = self.exact.entry(key).or_default();
        if !owners.contains(owner) {
            owners.push(owner.clone());
        }
    }

    fn add_named(&mut self, key: String, owner: &Owner) {
        let owners = self.named.entry(key.clone()).or_default();
        if !owners.contains(owner) {
            owners.push(owner.clone());
        }
        self.add(key, owner);
    }

    /// Who owns a file or folder called `name`.
    pub fn of(&self, name: &str) -> Vec<Owner> {
        let mut found: Vec<Owner> = Vec::new();
        let mut add = |owners: Option<&Vec<Owner>>| {
            for owner in owners.into_iter().flatten() {
                if !found.contains(owner) {
                    found.push(owner.clone());
                }
            }
        };
        for key in name_keys(name) {
            add(self.exact.get(&key));
            // Starting with a program's or package's name: katemetainfos → kate.
            for end in (MIN_PREFIX..key.len()).filter(|&end| key.is_char_boundary(end)) {
                add(self.named.get(&key[..end]));
            }
        }
        found
    }

    /// A program or package whose name contains a name `name` goes by, or is contained in it,
    /// which makes `name` less surely an orphan: `systemmonitor` and `plasma-systemmonitor`.
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

/// The names the files and folders called `names` go by, for [`Owners::from_pacman`].
pub fn wanted<'a>(names: impl IntoIterator<Item = &'a str>) -> HashSet<String> {
    names.into_iter().flat_map(name_keys).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(name: &str) -> Info {
        Info {
            name: name.into(),
            ..Info::default()
        }
    }

    fn package(name: &str) -> Owner {
        Owner::Package(name.into())
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
        assert_eq!(owners.of("nvim"), [package("neovim")]);
        assert_eq!(owners.of("fish"), [package("fish")]);
        assert_eq!(owners.of("katemetainfos"), [package("kate")]);
        assert_eq!(owners.of("htop"), [package("htop")]);
        assert_eq!(owners.of(".htoprc"), [package("htop")]);
        assert!(owners.of("gimp").is_empty());
        // Not every installed path counts: only those that were wanted.
        assert!(owners.of("share").is_empty());
    }

    #[test]
    fn short_names_are_not_prefixes() {
        let installed = [info("kio")];
        let owners = Owners::from_pacman(&installed, "", &HashSet::new());
        assert!(owners.of("kiosk-browser").is_empty());
        assert_eq!(owners.of("kiorc"), [package("kio")]);
    }

    #[test]
    fn names_close_to_a_package() {
        let installed = [info("plasma-systemmonitor")];
        let owners = Owners::from_pacman(&installed, "", &HashSet::new());
        assert!(owners.of("systemmonitorrc").is_empty());
        assert_eq!(
            owners.close_to("systemmonitorrc").as_deref(),
            Some("plasma-systemmonitor")
        );
        assert_eq!(owners.close_to("gimp"), None);
    }
}
