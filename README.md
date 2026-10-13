# vole

**A terminal tool to clean, analyze and maintain Arch Linux, in one binary.**

vole brings the pacman cache, AUR helper cache, systemd journal, user caches, orphaned packages, **orphaned configs in your home folder**, a disk explorer and a system dashboard into a single TUI. You navigate it with the arrow keys or Vim keys, and it never removes anything without showing you first.

It's inspired by [Mole](https://github.com/tw93/Mole) on macOS, but built for Arch: it knows about pacman, the AUR and systemd instead of treating Linux as generic.

> [!WARNING]
> **vole is in early development.** `vole clean`, `vole orphans`, `vole restore`, `vole analyze` and `vole status` work; it isn't on the AUR yet. See [Status](#status) for what works today and [ROADMAP.md](ROADMAP.md) for what's next.

---

## Why

Keeping an Arch system tidy today means remembering and combining several tools:

```sh
paccache -rk2                       # pacman cache
yay -Sc                             # AUR helper cache
pacman -Qdtq | sudo pacman -Rns -   # orphaned packages
journalctl --vacuum-size=200M       # systemd logs
rm -rf ~/.cache/...                 # user caches, by hand
```

Even then, one mess is left: **when you uninstall a package, its configs and data stay in `~/.config` and `~/.local/share` forever**, and no tool tells you which folders no longer belong to anything. vole does.

The alternatives are separate tools (gdu, btop, paccache), scripts without a real interface, or projects built for other distros that ignore pacman.

## Modules

| Module | What it does |
|---|---|
| **`clean`** | Frees space from the pacman cache (keeps the last 2 versions of each installed package), the AUR helper cache, the systemd journal (shrunk to 200 MB), developer caches (pip, npm, cargo, go), everything else in `~/.cache`, and the trash. |
| **`orphans`** | Finds **orphaned packages** (including packages that only become orphans once others are removed) and **orphaned configs**: folders in `~/.config`, `~/.local/share`, `~/.local/state`, `~/.cache` and dotfiles in `~` that no installed program owns. |
| **`restore`** | Puts orphaned configs you removed back where they were, from their dated backup. |
| **`analyze`** | An interactive disk explorer, like ncdu or gdu. Covers the whole filesystem on the current disk. You can move what you pick in your home folder to the trash; outside it, vole shows how to free the space. |
| **`status`** | A real-time dashboard read from `/proc` and `/sys`: CPU per core, memory and swap, disks, network, temperatures, battery, the busiest processes, and a health panel (the pacman cache, the journal, orphaned packages, pending updates) with shortcuts to `clean` and `orphans`. |

### How orphaned packages are found

vole starts from `pacman -Qdtq`: packages installed as dependencies that nothing installed needs. It adds what removing them with `pacman -Rns` takes along (dependencies nothing else needs), then the packages that only become orphans after that, and shows each under the package that frees it. Selecting a package selects what goes with it, so what you select is exactly what `pacman -Rns` removes; vole checks that with pacman right before removing. Each package also lists its config files, which `-n` deletes even when you changed them.

### How orphaned configs are detected

vole looks at what is directly in `~/.config`, `~/.local/share`, `~/.local/state` and `~/.cache`, and at the dotfiles directly in `~` (symlinks are left alone). One such as `~/.config/foo` is an **orphan** when no **owner** claims it. A folder goes by more than its name (`dolphinstaterc` is `dolphin`'s, `org.kde.dolphin` too, `.python_history` is `python`'s), and vole checks:

1. **pacman packages**: package names, the programs they install, their `.desktop` files, and the files they install whose path names the folder (like `/usr/share/fish` for `~/.config/fish`)
2. **programs outside pacman**: on your `$PATH` and in `~/.local/bin` and `~/.cargo/bin`; a folder holding a folder on your `$PATH` (like `~/.nvm`) is in use too
3. **Flatpaks**, **AppImages** and your own **desktop entries** whose program exists
4. a built-in list of folders not named after their owner (e.g. `~/.config/Kitware` → `cmake`, `kdeglobals` → KDE), and of folders that are part of the desktop (`mimeapps.list`, `user-dirs.dirs`, …)
5. your own **Known Owners** file, for anything vole can't work out (see [Configuration](#configuration))

Detection is a heuristic, so each orphan gets a **confidence** level:

- **High**: no owner, unchanged for 3 months
- **Medium**: no owner, but changed in the last 3 months or the name is close to something installed
- **Low**: loose dotfiles directly in `~`

Removed orphaned configs aren't deleted. They move into a dated **backup** in `~/.local/share/vole/backups/`, which `vole restore` puts back where they were; it never overwrites anything there now. Backups 30 days old or more show up in later Plans, to delete for good. When you remove a package, vole also offers its leftover configs straight away.

### How the disk explorer works

`vole analyze` scans the whole filesystem from `/`, reading many folders at once, and shows a folder at a time, largest first, each entry with its share of the folder. It stays on the disk `/` is on: other partitions and btrfs subvolumes of that disk (`/boot`, a separate `/home`) are scanned with it, while other disks, USB disks, network shares and what isn't on a disk (`/proc`, `/sys`, `/run`, `/tmp`) are listed but left out. Your home folder is always scanned, wherever it is mounted from (another disk, a systemd-homed image), unless it is a network share. `o` scans other disks too, USB disks among them; network shares are never looked into. Sizes are the space used on disk, and a file with several hard links counts once. A folder vole may not read is marked, and the folders holding it show "at least" (`≥`).

- **Inside your home folder** you can select files and folders and move them to the desktop trash (`gio trash`), where your file manager can put them back until the trash is emptied. Anything can be picked, the Protected List included, but never Sealed Paths, a trash folder, vole's own folders, or what is mounted (a mount point, or a folder a bind mount shows elsewhere), nor what holds any of these. On screen, what can't be picked has no box, and says why.
- **Outside it** vole only shows. The line under the list says how to free the space: known folders have their own tool (`vole clean` for the pacman cache and the journal, `docker system prune`, `flatpak uninstall --unused`, …), and for anything else, `pacman -Qo` tells which package it comes from, and whether other packages need it.

### How the dashboard works

`vole status` reads `/proc` and `/sys` every second (`status_refresh_ms`), on a thread of its own, and only while it is on screen; `+` and `-` make it read less or more often, from 250 ms to 5 s. Graphs show the CPU, the memory and the network over time, smoothed so they look as calm at any interval; the numbers are as read.

- **CPU**: each CPU's use as a bar, or, when that wouldn't fit, a cell each, from ▁ idle to █ busy.
- **Disks**: how full each filesystem on a disk is, once even when it is mounted at several paths (btrfs subvolumes, bind mounts), and what the disks read and write each second. Network shares are never asked, so one that doesn't answer can't hold the dashboard up.
- **Network**: what the interfaces leading out of the computer receive and send. Bridges, VPNs and containers' interfaces are left out: what goes through them goes through those too.
- **Temperatures and battery**: what each sensor chip measures first (a CPU's whole package, an NVMe disk's overall temperature), in bold once it is too hot, and how long each battery lasts or takes to charge. A wireless mouse's battery doesn't show.
- **Processes**: the busiest, by CPU or by the memory they hold.
- **Health**: the pacman cache, the journal, how many orphaned packages `vole orphans` would list, and how many updates are pending, measured each time the dashboard opens. `c` opens `clean` and `o` `orphans`; leaving them comes back to the dashboard. Pending updates come from `checkupdates`, which needs the network: it syncs a copy of the package databases in a folder of its own, never pacman's, so it can't lead to a partial upgrade. It runs at most every half hour.

## Safety

vole deletes files, so safety comes first.

- **Nothing is removed without a plan.** Every module first builds a **Plan**: the exact list of items and how much space each one frees. You select what to remove and confirm.
- **Nothing is pre-selected.** Safe items (caches that are recreated automatically) are marked as such, but you still pick them yourself.
- **Removal depends on the kind of item:**

  | Kind | What happens |
  |---|---|
  | Regenerable (caches) | Deleted permanently |
  | Orphaned config | Moved to a dated backup in `~/.local/share/vole/backups/`, restorable |
  | Orphaned package | `pacman -Rns`; the plan shows the `/etc` files that go with it |
  | Picked by hand in `analyze` | Moved to the desktop trash, where your file manager can put it back |
  | Trash | Emptying it is its own item, never automatic |

- **Items in use are flagged.** If a running program is using an item, removing it needs an extra confirmation.
- **Three layers of path rules:**
  - **Allowed Roots**: the only places modules look for things to propose.
  - **Protected List**: paths no module will ever propose, such as browser and mail profiles and your project folders. You can add your own.
  - **Sealed Paths**: SSH and GPG keys, password stores and keyrings, credentials (`.netrc`, `.pgpass`, cloud and Kubernetes credentials, the tokens of cargo, gh and npm, …) and crypto wallets, also where `GNUPGHOME`, `PASSWORD_STORE_DIR` or the XDG variables put them. vole never removes these, not even by hand.
- **Everything is logged.** Every removed item is recorded in `~/.local/state/vole/log.jsonl`.
- **Your projects are left alone.** vole has no module that deletes `node_modules` or `target/` automatically ([why](docs/adr/0002-no-purge-module.md)).

### Privileges

vole **runs as your normal user and refuses to run as root.** It only removes files itself inside your home folder; what you pick in `analyze` goes to the trash through `/usr/bin/gio`, as you. System-level work (pacman cache, journal, packages) is done by the official tools, `/usr/bin/paccache`, `/usr/bin/journalctl` and `/usr/bin/pacman`, through `sudo`, after you confirm. The TUI pauses so `sudo` can ask for your password itself; vole never sees it.

Only validated numbers from your config reach those commands, never free text, so a tampered config file can't be turned into root access. The only other arguments are the names of the packages you picked, which come from pacman itself and are checked to be valid package names. Details: [ADR 0001](docs/adr/0001-privilege-model.md).

## Status

| Milestone | State |
|---|---|
| **M0**: project setup, TUI skeleton, config, CI | ✅ Done ([#1](https://github.com/danielramaa/vole/issues/1)) |
| **M1**: `clean` shows the Plan | ✅ Done ([#2](https://github.com/danielramaa/vole/issues/2)) |
| **M2**: `clean` removes selected items | ✅ Done ([#3](https://github.com/danielramaa/vole/issues/3)) |
| **M3**: `orphans`, packages | ✅ Done ([#4](https://github.com/danielramaa/vole/issues/4)) |
| **M4**: `orphans`, configs and backups | ✅ Done ([#5](https://github.com/danielramaa/vole/issues/5)) |
| **M5**: `analyze` | ✅ Done ([#6](https://github.com/danielramaa/vole/issues/6)) |
| **M6**: `status` | ✅ Done ([#7](https://github.com/danielramaa/vole/issues/7)) |
| **M7**: AUR release | ⏳ Next ([#8](https://github.com/danielramaa/vole/issues/8)) |

## Installation

vole isn't on the AUR yet (planned for M7). For now, build it from source.

Requirements: Rust **1.88** or newer, `pacman-contrib` (for `paccache`, and `checkupdates`, which also needs `fakeroot`) and `glib2` (for `gio`, which moves what you pick in `analyze` to the trash).

```sh
git clone https://github.com/danielramaa/vole.git
cd vole
cargo build --release
./target/release/vole
```

## Usage

```sh
vole            # open the menu
vole clean      # open a module directly
vole orphans
vole restore
vole analyze
vole status
vole --help
```

### Keys

| Key | Action |
|---|---|
| `↑` `↓` / `k` `j` | Move |
| `Enter` / `→` / `l` | Open |
| `q` / `Esc` / `←` / `h` | Back to the menu, or quit if the module was opened directly |
| `Ctrl+C` | Quit |

In `clean`:

| Key | Action |
|---|---|
| `Space` | Select or deselect the Item (on a Category, the whole Category) |
| `a` | Select or deselect every Item in the current Category |
| `Enter` | Review the selection, then `y` to remove it or `n` to go back. If an Item is in use by a running program, removing it takes `Y` (shift+y) on a second screen. While removing, `Esc` stops after the current Item. |

In `orphans`:

| Key | Action |
|---|---|
| `Space` | Select or deselect the package, config or old backup (on a heading, all of them). A package goes or stays together with the packages it takes along. |
| `a` | Select or deselect everything in the current section |
| `Enter` | Review the selection, then `y` to remove it or `n` to go back. Configs move into a backup; packages go with `sudo pacman -Rns` (pacman lists them again and asks once more). If a config is in use by a running program, it takes `Y` (shift+y) on a second screen. While moving configs and deleting backups, `Esc` stops after the current one. |

In `restore`:

| Key | Action |
|---|---|
| `Space` | Select or deselect the config (on a backup, all its configs) |
| `a` | Select or deselect every config of the current backup |
| `Enter` | Review the selection, then `y` to put it back or `n` to go back |

In `analyze`:

| Key | Action |
|---|---|
| `l` / `→` / `Enter` | Open the folder |
| `h` / `←` / `Backspace` | Go up a folder (at the top, `h` and `←` leave `analyze`) |
| `PgUp` `PgDn` / `g` `G` | Move a page, or to the first or last entry |
| `~` | Go to your home folder |
| `Space` | Select or deselect the file or folder, inside your home folder. A folder takes what is in it along. |
| `t` | Review the selection, then `y` to move it to the trash or `n` to go back. If something is in use by a running program, it takes `Y` (shift+y) on a second screen. While moving, `Esc` stops after the current one. |
| `o` | Scan again with other disks too, or without them, even while scanning (a disk that doesn't answer can hold a scan up) |
| `r` | Scan again, or start over while scanning |

In `status`:

| Key | Action |
|---|---|
| `+` / `-` | Read less or more often: every 250 ms, 500 ms, 1 s, 2 s or 5 s |
| `s` | List the processes holding the most memory instead of CPU, and back |
| `c` / `o` | Open `clean` or `orphans`; leaving them comes back to the dashboard |

## Configuration

vole reads `~/.config/vole/config.toml`. Every setting is optional; missing ones use the defaults below. Out-of-range values are rejected with an error. Only the numbers ever reach a command run with `sudo`; `protected_paths` only keeps things out of the Plan.

```toml
# Cached versions of each installed package to keep (1–10)
pacman_keep_versions = 2

# Size the systemd journal is shrunk to, in MB (50–10000)
journal_max_mb = 200

# Refresh interval of the status dashboard, in ms (250–5000)
status_refresh_ms = 1000

# Extra paths for the Protected List, absolute or starting with ~/
protected_paths = []
```

Your **Known Owners** go in `~/.config/vole/known-owners.toml`: which folder belongs to what, for folders vole can't work out itself. A folder listed there is never an orphan, and neither is a folder holding one or inside one, so like `protected_paths` the file can only keep things from being proposed.

```toml
"~/.config/my-tool" = "my-tool, installed by hand in ~/apps"
"~/.local/share/my-game" = "the game in ~/Games"
```

## Development

vole is written in **Rust** with **[Ratatui](https://ratatui.rs)** ([why](docs/adr/0003-rust-and-ratatui.md)).

```sh
cargo fmt
cargo clippy --all-targets -- -D warnings
cargo test
cargo run
```

CI runs the same checks, plus a build on Rust 1.88 to make sure the minimum version still works.

### Dev container (any OS)

vole only runs on Arch. On any other system (Windows, macOS, another Linux distro), use the Arch Linux container. All you need is [Docker](https://docs.docker.com/get-docker/).

```sh
docker compose run --rm dev      # opens a shell inside Arch, in the project folder
cargo run                        # then work as usual
```

Your project folder is shared with the container, so you can edit in your usual editor on the host. Inside, you're the normal user `dev` (password `dev`, for `sudo`). The container is also a safe, throwaway system for testing what vole deletes.

After changing `docker/Dockerfile`, rebuild with `docker compose build`.

### Project documents

- [CONTEXT.md](CONTEXT.md): the glossary. What vole means by Plan, Item, Orphaned Config, Owner, Protected List and so on. Code and issues use these words.
- [ROADMAP.md](ROADMAP.md): the milestones, in order.
- [docs/adr/](docs/adr/): the key decisions and why they were made:
  - [0001: Privilege model](docs/adr/0001-privilege-model.md)
  - [0002: No purge module](docs/adr/0002-no-purge-module.md)
  - [0003: Rust + Ratatui](docs/adr/0003-rust-and-ratatui.md)

## Acknowledgements

- [Mole](https://github.com/tw93/Mole): the inspiration, for its all-in-one design and safety practices
- [gdu](https://github.com/dundee/gdu) and [ncdu](https://dev.yorhel.nl/ncdu): disk analysis
- [btop](https://github.com/aristocratos/btop): the dashboard

## License

vole is released under the [MIT License](LICENSE).
