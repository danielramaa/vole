//! Known Owners: the user's own list of which Owner a folder belongs to, in
//! `~/.config/vole/known-owners.toml`, for folders vole can't work out itself:
//!
//! ```toml
//! "~/.config/my-tool" = "my-tool, installed by hand in ~/apps"
//! ```
//!
//! A folder listed there is owned, whatever is installed, and so is a folder holding one or inside
//! one. Like the Protected List, the file can only keep things from being proposed.

use std::collections::BTreeMap;
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

pub fn path() -> Option<PathBuf> {
    dirs::config_dir().map(|dir| dir.join("vole").join("known-owners.toml"))
}

/// The Known Owners at `path`, none when the file doesn't exist: each folder, the folders above
/// it resolved, and what owns it.
pub fn load(path: &Path, home: &Path) -> Result<Vec<(PathBuf, String)>> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err).with_context(|| format!("reading {}", path.display())),
    };
    parse(&text, home).with_context(|| format!("reading {}", path.display()))
}

fn parse(text: &str, home: &Path) -> Result<Vec<(PathBuf, String)>> {
    let entries: BTreeMap<String, String> = toml::from_str(text)?;
    let mut known = Vec::new();
    for (folder, owner) in entries {
        let Some(relative) = folder.strip_prefix("~/").filter(|r| !r.is_empty()) else {
            bail!("{folder:?}: folders must start with ~/");
        };
        if owner.trim().is_empty() {
            bail!("{folder:?}: say what owns it");
        }
        known.push((resolved(&home.join(relative)), owner));
    }
    Ok(known)
}

/// `path` with the folders above it resolved, as the scan sees the folders it looks at.
fn resolved(path: &Path) -> PathBuf {
    let parent = path
        .parent()
        .and_then(|parent| fs::canonicalize(parent).ok());
    match (parent, path.file_name()) {
        (Some(parent), Some(name)) => parent.join(name),
        _ => path.to_path_buf(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testdir::TestDir;

    #[test]
    fn reads_folders_and_their_owners() {
        let home = TestDir::new();
        home.dir(".config");
        let text = r#"
"~/.config/my-tool" = "my-tool in ~/apps"
"~/.local/share/game/" = "the game"
"#;
        let known = parse(text, home.path()).unwrap();
        assert_eq!(
            known,
            [
                (
                    home.path().join(".config/my-tool"),
                    "my-tool in ~/apps".into()
                ),
                (home.path().join(".local/share/game"), "the game".into()),
            ]
        );
    }

    #[test]
    fn rejects_folders_outside_home_and_empty_owners() {
        let home = TestDir::new();
        for text in [
            r#""/etc/foo" = "bar""#,
            r#"".config/foo" = "bar""#,
            r#""~/" = "bar""#,
            r#""~/.config/foo" = """#,
            r#""~/.config/foo" = 1"#,
        ] {
            assert!(parse(text, home.path()).is_err(), "{text}");
        }
    }

    #[test]
    fn a_missing_file_has_no_owners() {
        let home = TestDir::new();
        let missing = home.path().join("known-owners.toml");
        assert!(load(&missing, home.path()).unwrap().is_empty());
        fs::write(&missing, "not = [toml").unwrap();
        let err = load(&missing, home.path()).unwrap_err();
        assert!(format!("{err:#}").contains("known-owners.toml"), "{err:#}");
    }
}
