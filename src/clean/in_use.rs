//! In Use detection: paths held by running processes, read from `/proc`.

use std::fs;
use std::path::{Path, PathBuf};

/// Executables and open files of every process the user can see.
pub fn open_paths() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let processes = fs::read_dir("/proc").into_iter().flatten().flatten();
    for process in processes {
        let is_pid = process
            .file_name()
            .to_str()
            .is_some_and(|name| name.bytes().all(|b| b.is_ascii_digit()));
        if !is_pid {
            continue;
        }
        let dir = process.path();
        paths.extend(link_target(&dir.join("exe")));
        let fds = fs::read_dir(dir.join("fd")).into_iter().flatten().flatten();
        paths.extend(fds.filter_map(|fd| link_target(&fd.path())));
    }
    paths.sort();
    paths.dedup();
    paths
}

/// The file a `/proc` link points to; `None` for sockets, pipes and the like.
fn link_target(link: &Path) -> Option<PathBuf> {
    let target = fs::read_link(link).ok()?;
    let text = target.to_str()?;
    let text = text.strip_suffix(" (deleted)").unwrap_or(text);
    text.starts_with('/').then(|| PathBuf::from(text))
}

/// Whether `path` or anything inside it is in `open`.
pub fn is_in_use(path: &Path, open: &[PathBuf]) -> bool {
    open.iter().any(|open| open.starts_with(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_folder_is_in_use_when_a_file_inside_is_open() {
        let open = [PathBuf::from("/h/.cache/app/db.sqlite")];
        assert!(is_in_use(Path::new("/h/.cache/app"), &open));
        assert!(is_in_use(Path::new("/h/.cache/app/db.sqlite"), &open));
        assert!(!is_in_use(Path::new("/h/.cache/ap"), &open));
        assert!(!is_in_use(Path::new("/h/.cache/other"), &open));
    }

    #[test]
    fn sees_this_process() {
        let exe = fs::canonicalize(std::env::current_exe().unwrap()).unwrap();
        assert!(is_in_use(&exe, &open_paths()));
    }
}
