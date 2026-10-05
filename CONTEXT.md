# Vole

A tool for cleaning, analyzing and maintaining a single user's Arch Linux system. It finds reclaimable space and leftover files, and removes them only after the user reviews and confirms.

## Language

### Planning and execution

**Plan**:
The list of Items a module proposes to remove, each with its size and kind, built before anything is touched. No Item starts selected; nothing is removed until the user selects Items and confirms.
_Avoid_: Preview, report, scan result

**Item**:
One entry in a Plan: a single path or package the user can select or deselect.
_Avoid_: Entry, candidate, target

**Category**:
A named group of Items within a module (e.g. pip cache, yay cache, user cache). Every path belongs to at most one Category; specific Categories claim their paths before general ones.
_Avoid_: Section, group, type

**In Use**:
A flag on an Item whose path is held by a running process. Removing it requires an extra confirmation.
_Avoid_: Busy, locked

**Dry run**:
Building and showing a Plan without applying it.
_Avoid_: Simulation, preview mode

### Item kinds

**Regenerable**:
An Item that software recreates on demand, such as caches. Marked as such in the Plan to signal it is safe, but still not pre-selected. Removing it deletes it permanently.
_Avoid_: Temporary, junk

**Orphaned Config**:
A directory or file under the user's config or data folders that no Owner claims. Detection is heuristic, so removing one moves it into a Backup rather than deleting it.
_Avoid_: Leftover, dangling config, stale data

**Confidence**:
How likely an Orphaned Config really has no Owner: High, Medium or Low. Loose dotfiles directly in the home directory are always Low.
_Avoid_: Score, certainty, probability

**Orphaned Package**:
A pacman package installed as a dependency that nothing installed still requires, including packages that only become orphaned once other Orphaned Packages are removed.
_Avoid_: Unused package

**Backup**:
A dated snapshot, kept by Vole, of Orphaned Configs it removed, from which they can be restored to their original paths.
_Avoid_: Trash, archive

**Operation Log**:
The append-only record of every Item vole actually removed, and the outcome.
_Avoid_: History, audit trail

### Ownership

**Owner**:
Something installed that accounts for a config or data folder: a pacman package, a Flatpak, an AppImage, a user-installed binary or desktop entry, or an entry in the Known Owners map.
_Avoid_: Parent, source app

**Known Owners**:
A user-maintained mapping that says which Owner a folder belongs to, for cases the detectors can't work out.
_Avoid_: Manual mappings, overrides

### Safety

**Protected List**:
Paths no module will ever propose as Items; ships with defaults (browser profiles, project folders) and the user can extend it. The user can still hand-pick them in the disk explorer.
_Avoid_: Whitelist, allowlist, exclusions

**Sealed Paths**:
Paths vole never removes by any means, including hand-picking: secrets and keys such as SSH, GPG, password stores and keyrings. Takes precedence over every other rule.
_Avoid_: Protected (for this meaning), locked

**Allowed Roots**:
The only locations where modules may propose Items on their own. Items the user picks by hand in the disk explorer may come from anywhere in the home directory, and go to the desktop trash.
_Avoid_: Known paths, safe dirs, whitelist
