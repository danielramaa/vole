//! Memory and swap, from `/proc/meminfo`.

use std::fs;

/// How much memory and swap there are, and how much of them is free, in bytes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Memory {
    pub total: u64,
    /// What programs can still get without swapping: free memory, and caches that can go.
    pub available: u64,
    pub swap_total: u64,
    pub swap_free: u64,
}

impl Memory {
    pub fn read() -> Option<Memory> {
        parse(&fs::read_to_string("/proc/meminfo").ok()?)
    }

    pub fn used(&self) -> u64 {
        self.total.saturating_sub(self.available)
    }

    pub fn swap_used(&self) -> u64 {
        self.swap_total.saturating_sub(self.swap_free)
    }
}

/// Reads lines such as `MemTotal:       15977432 kB`.
fn parse(text: &str) -> Option<Memory> {
    let field = |name: &str| -> Option<u64> {
        let value = text
            .lines()
            .find_map(|line| line.strip_prefix(name)?.strip_prefix(':'))?;
        let kib: u64 = value.split_whitespace().next()?.parse().ok()?;
        Some(kib * 1024)
    };
    Some(Memory {
        total: field("MemTotal")?,
        available: field("MemAvailable")?,
        swap_total: field("SwapTotal").unwrap_or(0),
        swap_free: field("SwapFree").unwrap_or(0),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_memory_and_swap() {
        let memory = parse(
            "MemTotal:       15977432 kB\n\
             MemFree:          789280 kB\n\
             MemAvailable:    4836200 kB\n\
             SwapCached:            0 kB\n\
             SwapTotal:       8388604 kB\n\
             SwapFree:        8376528 kB\n",
        )
        .unwrap();
        assert_eq!(memory.total, 15_977_432 * 1024);
        assert_eq!(memory.used(), (15_977_432 - 4_836_200) * 1024);
        assert_eq!(memory.swap_used(), (8_388_604 - 8_376_528) * 1024);
        let no_swap = parse("MemTotal: 2048 kB\nMemAvailable: 1024 kB\n").unwrap();
        assert_eq!(no_swap.swap_total, 0);
        assert_eq!(parse("MemTotal: 2048 kB\n"), None);
    }

    #[test]
    fn reads_this_system() {
        let memory = Memory::read().unwrap();
        assert!(memory.total > 0 && memory.used() <= memory.total);
    }
}
