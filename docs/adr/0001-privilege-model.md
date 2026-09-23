# Runs as the user; root only through official tools

Vole refuses to run as root and only removes files itself inside the user's home directory. Anything outside it (pacman cache, systemd journal, orphaned packages) is handled by calling the official tool (`/usr/bin/paccache`, `/usr/bin/journalctl`, `/usr/bin/pacman`) through `sudo`, after the user confirms. The TUI pauses so `sudo` can ask for the password itself; vole never handles it. Binaries are called by absolute path, and only validated numbers from the user config (versions to keep, journal size) reach a root command, never free text. That means malware running as the user can't turn a vole config edit into root access.

## Considered Options

- **Run as root**: `$HOME` resolves to root's, files end up owned by root, and one bug could break the system.
- **Root-owned config in `/etc/vole/`, editable only with `sudo`**: rejected. Once root commands only take validated numbers, there's nothing left to protect. Protecting the Protected List with `sudo` doesn't stop an attacker who can already delete the user's files with `rm`.
- **Deleting system files directly**: rejected. `paccache` and `journalctl` already handle edge cases (keeping versions, safe journal rotation) we'd otherwise have to reproduce.
