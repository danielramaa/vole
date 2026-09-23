# Rust + Ratatui

Vole is written in Rust with Ratatui. The main reason is safety: every Item kind (Regenerable, Orphaned Config, Orphaned Package, hand-picked in the disk explorer) has exactly one allowed way to be removed. With Rust enums and exhaustive `match`, adding a new kind without deciding how it is removed fails to compile. Ratatui also ships the charts, gauges and sparklines the status dashboard needs, and the author wants to get better at systems programming. The cost is slower early development than Go.

## Considered Options

- **Go + Bubble Tea**: the fastest to build, and the language Mole uses, with built-in support for pausing the TUI to run `sudo`. Rejected because it has no exhaustive enums and its charts come from third-party packages.
- **C++ + FTXUI**: memory-safety risk is unacceptable in a tool that deletes files.
- **Zig + libvaxis**: attractive for learning, but pre-1.0 language churn is a poor fit for a long-lived AUR package.
- **Python + Textual, TypeScript + Ink**: break the "fast, single binary" principle.
