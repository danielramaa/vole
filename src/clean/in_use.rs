//! In Use detection: paths held by running processes, read from `/proc`.

use std::fs;
use std::path::{Path, PathBuf};

/// Executables, working folders, open files and memory-mapped files of every process the user
/// can see.
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
        paths.extend(link_target(&dir.join("cwd")));
        // Memory-mapped files, which often stay in use after their fd is closed.
        if let Ok(maps) = fs::read_to_string(dir.join("maps")) {
            paths.extend(maps.lines().filter_map(mapped_path));
        }
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

/// The file of a `/proc/<pid>/maps` line, e.g.
/// `7f2c1a000000-7f2c1a100000 r--s 00000000 fe:01 1234   /home/me/.cache/fontconfig/x.cache-9`.
fn mapped_path(line: &str) -> Option<PathBuf> {
    // Five fields, then the path, which may itself contain spaces.
    let mut rest = line;
    for _ in 0..5 {
        rest = rest.trim_start().split_once(' ')?.1;
    }
    let path = rest.trim_start();
    let path = path.strip_suffix(" (deleted)").unwrap_or(path);
    path.starts_with('/').then(|| PathBuf::from(path))
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
    fn reads_mapped_files() {
        let line = "7f2c1a000000-7f2c1a100000 r--s 00000000 fe:01 1234       /h/.cache/font config/x (deleted)";
        assert_eq!(
            mapped_path(line),
            Some(PathBuf::from("/h/.cache/font config/x"))
        );
        assert_eq!(
            mapped_path("7ffd5a000000-7ffd5a021000 rw-p 00000000 00:00 0      [stack]"),
            None
        );
        assert_eq!(
            mapped_path("7f2c1a000000-7f2c1a100000 rw-p 00000000 00:00 0"),
            None
        );
    }

    #[test]
    fn sees_this_process() {
        let exe = fs::canonicalize(std::env::current_exe().unwrap()).unwrap();
        assert!(is_in_use(&exe, &open_paths()));
    }
}
