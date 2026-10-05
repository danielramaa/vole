# vole

**A terminal tool to clean, analyze and maintain Arch Linux, in one binary.**

vole brings the pacman cache, AUR helper cache, systemd journal, user caches, orphaned packages, **orphaned configs in your home folder**, a disk explorer and a system dashboard into a single TUI. You navigate it with the arrow keys or Vim keys, and it never removes anything without showing you first.

It's inspired by [Mole](https://github.com/tw93/Mole) on macOS, but built for Arch: it knows about pacman, the AUR and systemd instead of treating Linux as generic.

> [!WARNING]
> **vole is in early development.** `vole clean` and the orphaned packages in `vole orphans` work; the rest is still to come. See [Status](#status) for what works today and [ROADMAP.md](ROADMAP.md) for what's next.

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
| **`analyze`** | An interactive disk explorer, like ncdu or gdu. Covers the whole filesystem on the current disk. You can delete files in your home folder; outside it, vole shows the safe way to free the space. |
| **`status`** | A real-time dashboard read from `/proc` and `/sys`: CPU per core, memory, disk, network, temperatures, top processes, battery, and a system-health panel (cache sizes, orphans, journal size, pending updates). |

### How orphaned packages are found

vole starts from `pacman -Qdtq`: packages installed as dependencies that nothing installed needs. It adds what removing them with `pacman -Rns` takes along (dependencies nothing else needs), then the packages that only become orphans after that, and shows each under the package that frees it. Selecting a package selects what goes with it, so what you select is exactly what `pacman -Rns` removes; vole checks that with pacman right before removing. Each package also lists its config files, which `-n` deletes even when you changed them.

### How orphaned configs are detected

A folder such as `~/.config/foo` is an **orphan** when no **owner** claims it. vole checks:

1. **pacman packages**: package names, their binaries, their `.desktop` files, and files that mention the folder
2. **Flatpaks**, binaries in `~/.local/bin` and `~/.cargo/bin`, and **AppImages**
3. a built-in list of known name mismatches (e.g. `~/.mozilla` → `firefox`, `~/.config/Code` → VS Code)
4. your own **Known Owners** file, for anything vole can't work out

Detection is a heuristic, so each orphan gets a **confidence** level:

- **High**: no owner, unchanged for months
- **Medium**: no owner, but changed recently or the name is close to an installed package
- **Low**: loose dotfiles directly in `~`

Removed orphaned configs aren't deleted. They go into a dated **backup** that you can restore. When you remove a package, vole also offers its leftover configs straight away.

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
  | Picked by hand in `analyze` | Moved to the desktop trash |
  | Trash | Emptying it is its own item, never automatic |

- **Items in use are flagged.** If a running program is using an item, removing it needs an extra confirmation.
- **Three layers of path rules:**
  - **Allowed Roots**: the only places modules look for things to propose.
  - **Protected List**: paths no module will ever propose, such as browser profiles and your project folders. You can add your own.
  - **Sealed Paths**: SSH and GPG keys, password stores and keyrings. vole never removes these, not even by hand.
- **Everything is logged.** Every removed item is recorded in `~/.local/state/vole/log.jsonl`.
- **Your projects are left alone.** vole has no module that deletes `node_modules` or `target/` automatically ([why](docs/adr/0002-no-purge-module.md)).

### Privileges

vole **runs as your normal user and refuses to run as root.** It only removes files itself inside your home folder. System-level work (pacman cache, journal, packages) is done by the official tools, `/usr/bin/paccache`, `/usr/bin/journalctl` and `/usr/bin/pacman`, through `sudo`, after you confirm. The TUI pauses so `sudo` can ask for your password itself; vole never sees it.

Only validated numbers from your config reach those commands, never free text, so a tampered config file can't be turned into root access. The only other arguments are the names of the packages you picked, which come from pacman itself and are checked to be valid package names. Details: [ADR 0001](docs/adr/0001-privilege-model.md).

## Status

| Milestone | State |
|---|---|
| **M0**: project setup, TUI skeleton, config, CI | ✅ Done ([#1](https://github.com/danielramaa/vole/issues/1)) |
| **M1**: `clean` shows the Plan | ✅ Done ([#2](https://github.com/danielramaa/vole/issues/2)) |
| **M2**: `clean` removes selected items | ✅ Done ([#3](https://github.com/danielramaa/vole/issues/3)) |
| **M3**: `orphans`, packages | ✅ Done ([#4](https://github.com/danielramaa/vole/issues/4)) |
| **M4**: `orphans`, configs and backups | ⏳ Next ([#5](https://github.com/danielramaa/vole/issues/5)) |
| **M5**: `analyze` | [#6](https://github.com/danielramaa/vole/issues/6) |
| **M6**: `status` | [#7](https://github.com/danielramaa/vole/issues/7) |
| **M7**: AUR release | [#8](https://github.com/danielramaa/vole/issues/8) |

## Installation

vole isn't on the AUR yet (planned for M7). For now, build it from source.

Requirements: Rust **1.88** or newer, and `pacman-contrib` (for `paccache`).

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
| `Space` | Select or deselect the package (on the heading, all of them), together with the packages that go or stay with it |
| `a` | Select or deselect every package |
| `Enter` | Review the selection, then `y` to remove it with `sudo pacman -Rns` (pacman lists the packages again and asks once more) or `n` to go back |

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

Not chosen yet. A license will be added before the first release.
