# Roadmap

Each milestone ends with something usable. Terms follow `CONTEXT.md`.

1. **`clean`, Plan only**: build and show the Plan (Categories, Items, sizes, Regenerable/In Use marks) in the TUI, with selection, and remove nothing.
2. **`clean`, removing**: confirm and remove selected Items. Pause the TUI for `sudo` when running `paccache` and `journalctl`. Enforce the Protected List, Sealed Paths and Allowed Roots, and write to the Operation Log.
3. **`orphans`, packages**: Orphaned Packages, including packages that only become orphans once others are removed, removed with `pacman -Rns`, showing the `/etc` files that go with them.
4. **`orphans`, configs**: Owner detection, Known Owners, Confidence levels, Backups and `restore`. Offer a package's leftover configs right after the package is removed.
5. **`analyze`**: disk explorer across the whole filesystem on one disk. Items hand-picked inside the home directory go to the desktop trash; outside it, view-only, with a tip on how to free the space.
6. **`status`**: CPU, memory, disk, network, temperatures, top processes, battery and the system-health panel. 1 s refresh, adjustable.
7. **Publish to the AUR.**
