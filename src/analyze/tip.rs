//! How to free the space outside the home folder, where vole only shows what uses it: the tool
//! that frees a known folder, or the package a path comes from.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::pacman::{self, Info};

/// A folder a tool of its own frees, or that is best left alone.
struct Known {
    /// Relative to `/`. Anything in it gets its tip.
    folder: &'static str,
    tip: &'static str,
    /// The tip of the folder holding it, which it is most of.
    holder: Option<&'static str>,
}

const KNOWN: &[Known] = &[
    Known {
        folder: "var/cache/pacman/pkg",
        tip: "pacman's package cache: vole clean trims it.",
        holder: Some("Holds pacman's package cache, which vole clean trims."),
    },
    Known {
        folder: "var/log/journal",
        tip: "The systemd journal: vole clean shrinks it.",
        holder: Some("Holds the systemd journal, which vole clean shrinks."),
    },
    Known {
        folder: "var/lib/pacman",
        tip: "pacman's record of what is installed: keep it.",
        holder: None,
    },
    Known {
        folder: "var/lib/docker",
        tip: "Docker's images and containers: docker system prune removes the unused ones.",
        holder: None,
    },
    Known {
        folder: "var/lib/containers",
        tip: "Podman's images and containers: podman system prune removes the unused ones.",
        holder: None,
    },
    Known {
        folder: "var/lib/flatpak",
        tip: "Flatpak's apps and runtimes: flatpak uninstall --unused removes the runtimes no app needs.",
        holder: None,
    },
    Known {
        folder: "var/lib/libvirt/images",
        tip: "Virtual machine disks: delete the machines you don't need in virt-manager.",
        holder: Some(
            "Holds virtual machine disks: delete the machines you don't need in virt-manager.",
        ),
    },
    Known {
        folder: "var/lib/systemd/coredump",
        tip: "Core dumps of programs that crashed: coredumpctl lists them, and systemd deletes them after a few days.",
        holder: Some(
            "Holds core dumps of programs that crashed, which systemd deletes after a few days.",
        ),
    },
    Known {
        folder: "var/lib/mysql",
        tip: "MariaDB's or MySQL's databases: drop the ones you don't need from the database itself.",
        holder: None,
    },
    Known {
        folder: "var/lib/postgres",
        tip: "PostgreSQL's databases: drop the ones you don't need from the database itself.",
        holder: None,
    },
    Known {
        folder: "var/tmp",
        tip: "Temporary files kept across reboots: systemd deletes the ones unused for 30 days.",
        holder: None,
    },
    Known {
        folder: "usr/lib/modules",
        tip: "Kernel modules: they come and go with the kernel packages.",
        holder: None,
    },
    Known {
        folder: "usr/lib/firmware",
        tip: "Firmware your devices load as they start: keep it.",
        holder: None,
    },
    Known {
        folder: "boot",
        tip: "The kernels and the boot loader: pacman and the boot loader look after them.",
        holder: None,
    },
];

/// The tip for `path`, outside the home folder `home`, when no command has to run for it.
pub fn known(path: &Path, home: &Path) -> Option<&'static str> {
    known_with(path, home, other_homes())
}

/// [`known`], where `others` are the home folders of the system's other people.
fn known_with(path: &Path, home: &Path, others: &[PathBuf]) -> Option<&'static str> {
    if path == home {
        return Some("Your home folder: open it to select what to move to the trash.");
    }
    if home.starts_with(path) {
        return Some("Your home folder is in here; ~ goes straight to it.");
    }
    let relative = path.strip_prefix("/").ok()?;
    if let Some(known) = KNOWN
        .iter()
        .find(|known| relative.starts_with(known.folder))
    {
        return Some(known.tip);
    }
    // What a folder holds may matter more than what it comes from: /var/cache/pacman is
    // pacman's, but its size is the package cache.
    if let Some(holder) = KNOWN
        .iter()
        .filter(|known| Path::new(known.folder).parent() == Some(relative))
        .find_map(|known| known.holder)
    {
        return Some(holder);
    }
    others
        .iter()
        .any(|other| path.starts_with(other))
        .then_some("Another user's home folder: only they, or root, can free this space.")
}

/// The home folders of the system's other people, and root's, from /etc/passwd, read once.
fn other_homes() -> &'static [PathBuf] {
    static HOMES: OnceLock<Vec<PathBuf>> = OnceLock::new();
    HOMES.get_or_init(|| {
        let passwd = fs::read_to_string("/etc/passwd").unwrap_or_default();
        people_homes(&passwd, rustix::process::getuid().as_raw())
    })
}

/// The home folders `passwd` gives root and the people other than the user `me`. System users
/// are left out: their homes are often their programs' folders, like /var/lib/colord.
fn people_homes(passwd: &str, me: u32) -> Vec<PathBuf> {
    passwd
        .lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split(':').collect();
            let uid: u32 = fields.get(2)?.parse().ok()?;
            let home = Path::new(fields.get(5)?);
            let person = uid == 0 || (1000..=60000).contains(&uid);
            (person && uid != me && home.is_absolute() && home != Path::new("/"))
                .then(|| home.to_path_buf())
        })
        .collect()
}

/// How to free `path` (a folder when `folder`), from what pacman says about it. pacman takes a
/// moment.
pub fn from_pacman(path: &Path, folder: bool) -> String {
    match pacman::owners(path) {
        Ok(owners) => {
            let info = match owners.as_slice() {
                [owner] => pacman::package_info(owner).ok(),
                _ => None,
            };
            package_tip(&owners, info.as_ref(), folder)
        }
        Err(err) => format!("pacman couldn't say what it comes from: {err:#}"),
    }
}

/// The tip for a path the packages `owners` own, where `info` is about the only one. Of a folder,
/// pacman only tells who made it: what programs put in it later isn't the package's.
fn package_tip(owners: &[String], info: Option<&Info>, folder: bool) -> String {
    let from = if folder {
        format!("The folder comes from the package {}", owners.join(", "))
    } else {
        format!("From the package {}", owners.join(", "))
    };
    match owners {
        [] => "No package owns it: a program made it, or it was put there by hand.".into(),
        [owner] => match info {
            Some(info) if !info.required_by.is_empty() => {
                let needed = match info.required_by.as_slice() {
                    [one] => format!("{one} needs"),
                    [one, two] => format!("{one} and {two} need"),
                    all => format!("{} installed packages need", all.len()),
                };
                format!("{from}, which {needed}.")
            }
            // What vole orphans lists: installed as a dependency of something now gone.
            Some(info) if info.dependency && info.optional_for.is_empty() => {
                format!("{from}, which nothing installed needs: vole orphans removes it.")
            }
            Some(_) if folder => format!(
                "{from}: if you don't need it, sudo pacman -Rns {owner} removes what it put here."
            ),
            Some(_) => {
                format!("{from}: if you don't need it, sudo pacman -Rns {owner} removes it.")
            }
            None => format!("{from}."),
        },
        _ if folder => format!(
            "Holds files of {} packages: open it to see whose.",
            owners.len()
        ),
        _ => format!("Shared by the packages {}.", owners.join(", ")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOME: &str = "/home/me";

    fn known_tip(path: &str) -> Option<&'static str> {
        known_with(
            Path::new(path),
            Path::new(HOME),
            &[PathBuf::from("/home/other")],
        )
    }

    #[test]
    fn knows_the_folders_with_a_tool_of_their_own() {
        assert_eq!(
            known_tip("/var/cache/pacman/pkg"),
            Some("pacman's package cache: vole clean trims it.")
        );
        assert!(
            known_tip("/var/lib/docker/overlay2")
                .unwrap()
                .contains("docker system prune")
        );
        assert!(
            known_tip("/usr/lib/modules/6.17.1-arch1-1")
                .unwrap()
                .contains("kernel")
        );
        assert_eq!(known_tip("/var/lib/dockerd"), None);
        assert_eq!(known_tip("/usr/lib/firefox"), None);
        assert!(known_tip("/var/lib/mysql").unwrap().contains("databases"));
    }

    #[test]
    fn says_what_a_folder_holds_before_whose_it_is() {
        assert_eq!(
            known_tip("/var/cache/pacman"),
            Some("Holds pacman's package cache, which vole clean trims.")
        );
        assert!(known_tip("/var/log").unwrap().contains("journal"));
        // Only what a folder is mostly made of: /var/cache holds much more.
        assert_eq!(known_tip("/var/cache"), None);
        assert_eq!(known_tip("/var/lib"), None);
    }

    #[test]
    fn knows_the_home_folders() {
        assert!(
            known_tip("/home")
                .unwrap()
                .starts_with("Your home folder is in here")
        );
        assert!(
            known_tip("/home/me")
                .unwrap()
                .starts_with("Your home folder: open it")
        );
        assert!(
            known_tip("/home/other/big")
                .unwrap()
                .starts_with("Another user's")
        );
        // Not everything next to the home folder is someone else's.
        assert_eq!(known_tip("/home/lost+found"), None);
        assert_eq!(known_tip("/home/me.homedir"), None);
    }

    #[test]
    fn reads_who_else_has_a_home_folder() {
        let passwd = "\
root:x:0:0::/root:/usr/bin/bash
nobody:x:65534:65534:Kernel Overflow User:/:/usr/bin/nologin
colord:x:977:977:Color management daemon:/var/lib/colord:/usr/bin/nologin
me:x:1000:1000::/home/me:/usr/bin/zsh
other:x:1001:1001::/data/other:/usr/bin/bash
";
        assert_eq!(
            people_homes(passwd, 1000),
            [PathBuf::from("/root"), PathBuf::from("/data/other")]
        );
    }

    fn package(required_by: &[&str], dependency: bool, optional_for: &[&str]) -> Info {
        Info {
            name: "foo".into(),
            dependency,
            required_by: required_by.iter().map(|p| p.to_string()).collect(),
            optional_for: optional_for.iter().map(|p| p.to_string()).collect(),
            ..Info::default()
        }
    }

    #[test]
    fn says_what_removing_the_package_takes() {
        let foo = ["foo".to_string()];
        let tip = |info: &Info| package_tip(&foo, Some(info), false);
        assert_eq!(
            tip(&package(&["bar"], true, &[])),
            "From the package foo, which bar needs."
        );
        assert_eq!(
            tip(&package(&["a", "b", "c"], false, &[])),
            "From the package foo, which 3 installed packages need."
        );
        assert_eq!(
            tip(&package(&[], true, &[])),
            "From the package foo, which nothing installed needs: vole orphans removes it."
        );
        // Still optional for an installed package: not an orphan, but nothing needs it.
        assert!(tip(&package(&[], true, &["bar"])).contains("sudo pacman -Rns foo"));
        assert_eq!(
            tip(&package(&[], false, &[])),
            "From the package foo: if you don't need it, sudo pacman -Rns foo removes it."
        );
        assert_eq!(package_tip(&foo, None, false), "From the package foo.");
        // Of a folder, the package made the folder, not everything in it.
        assert_eq!(
            package_tip(&foo, Some(&package(&[], false, &[])), true),
            "The folder comes from the package foo: if you don't need it, \
             sudo pacman -Rns foo removes what it put here."
        );
        assert_eq!(
            package_tip(&foo, Some(&package(&["bar"], false, &[])), true),
            "The folder comes from the package foo, which bar needs."
        );
    }

    #[test]
    fn says_when_no_package_or_several_own_it() {
        assert!(package_tip(&[], None, false).starts_with("No package owns it"));
        let both = ["a".to_string(), "b".to_string()];
        assert_eq!(
            package_tip(&both, None, true),
            "Holds files of 2 packages: open it to see whose."
        );
        assert_eq!(
            package_tip(&both, None, false),
            "Shared by the packages a, b."
        );
    }
}
