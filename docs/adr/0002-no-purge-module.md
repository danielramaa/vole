# No build-artifact purge module

Unlike Mole, vole has no module that finds and deletes build artifacts (`node_modules`, `target/`, `.venv`) in project folders. Project folders are in the default Protected List, so no module ever proposes anything inside them. The user decided that vole should never automatically touch anything inside their projects, even regenerable files. Space used by projects can still be found in the disk explorer and removed there by hand, to the desktop trash.
