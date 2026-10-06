//! How to free the space outside the home folder, where vole only shows what uses it: the tool
//! that frees a known folder, or the package a path comes from.

use std::path::Path;

use crate::pacman::{self, Info};

/// Folders a tool of their own frees, or that are best left alone, relative to `/`. Anything in
/// them gets their tip.
const KNOWN: &[(&str, &str)] = &[
    (
        "var/cache/pacman/pkg",
        "pacman's package cache: vole clean trims it.",
    ),
    (
        "var/log/journal",
        "The systemd journal: vole clean shrinks it.",
    ),
    (
        "var/lib/pacman",
        "pacman's record of what is installed: keep it.",
    ),
    (
        "var/lib/docker",
        "Docker's images and containers: docker system prune removes the unused ones.",
    ),
    (
        "var/lib/containers",
        "Podman's images and containers: podman system prune removes the unused ones.",
    ),
    (
        "var/lib/flatpak",
        "Flatpak's apps and runtimes: flatpak uninstall --unused removes the runtimes no app needs.",
    ),
    (
        "var/lib/libvirt/images",
        "Virtual machine disks: delete the machines you don't need in virt-manager.",
    ),
    (
        "var/lib/systemd/coredump",
        "Core dumps of programs that crashed: coredumpctl lists them, and systemd deletes them after a few days.",
    ),
    (
        "var/tmp",
        "Temporary files kept across reboots: systemd deletes the ones unused for 30 days.",
    ),
    (
        "usr/lib/modules",
        "Kernel modules: they come and go with the kernel packages.",
    ),
    (
        "usr/lib/firmware",
        "Firmware your devices load as they start: keep it.",
    ),
    (
        "boot",
        "The kernels and the boot loader: pacman and the boot loader look after them.",
    ),
];

/// The tip for `path`, outside the home folder `home`, when no command has to run for it.
pub fn known(path: &Path, home: &Path) -> Option<&'static str> {
    if path == home {
        return Some("Your home folder: open it to select what to move to the trash.");
    }
    if home.starts_with(path) {
        return Some("Your home folder is in here; ~ goes straight to it.");
    }
    if let Some(homes) = home.parent().filter(|homes| *homes != Path::new("/"))
        && path.starts_with(homes)
    {
        return Some("Another user's home folder: only they, or root, can free this space.");
    }
    let relative = path.strip_prefix("/").ok()?;
    KNOWN
        .iter()
        .find(|(folder, _)| relative.starts_with(folder))
        .map(|(_, tip)| *tip)
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

/// The tip for a path the packages `owners` own, where `info` is about the only one.
fn package_tip(owners: &[String], info: Option<&Info>, folder: bool) -> String {
    match owners {
        [] => "No package owns it: a program made it, or it was put there by hand.".into(),
        [owner] => match info {
            Some(info) if !info.required_by.is_empty() => {
                let needed = match info.required_by.as_slice() {
                    [one] => format!("{one} needs"),
                    [one, two] => format!("{one} and {two} need"),
                    all => format!("{} installed packages need", all.len()),
                };
                format!("From the package {owner}, which {needed}.")
            }
            // What vole orphans lists: installed as a dependency of something now gone.
            Some(info) if info.dependency && info.optional_for.is_empty() => {
                format!(
                    "From the package {owner}, which nothing installed needs: vole orphans removes it."
                )
            }
            Some(_) => format!(
                "From the package {owner}: if you don't need it, sudo pacman -Rns {owner} removes it."
            ),
            None => format!("From the package {owner}."),
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
        known(Path::new(path), Path::new(HOME))
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
        // With the home folder right in /, nothing else in / is someone else's.
        assert_eq!(known(Path::new("/opt"), Path::new("/me")), None);
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
