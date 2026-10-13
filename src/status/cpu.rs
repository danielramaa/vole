//! CPU use, from the time `/proc/stat` says the CPUs spent busy and idle.

use std::fs;

/// Time the CPUs spent busy, and in all, in clock ticks.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Times {
    pub busy: u64,
    pub total: u64,
}

impl Times {
    /// The time spent between `earlier` and `self`.
    fn since(self, earlier: Times) -> Times {
        Times {
            busy: self.busy.saturating_sub(earlier.busy),
            total: self.total.saturating_sub(earlier.total),
        }
    }
}

/// The time spent by all CPUs together, and by each, by its number.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Cpu {
    pub all: Times,
    pub cores: Vec<(u32, Times)>,
}

impl Cpu {
    /// The time spent since the system started.
    pub fn read() -> Option<Cpu> {
        parse(&fs::read_to_string("/proc/stat").ok()?)
    }

    /// The time spent between `earlier` and `self`, by the CPUs both know.
    pub fn since(&self, earlier: &Cpu) -> Cpu {
        let cores = self.cores.iter().filter_map(|&(n, times)| {
            let (_, before) = earlier.cores.iter().find(|&&(m, _)| m == n)?;
            Some((n, times.since(*before)))
        });
        Cpu {
            all: self.all.since(earlier.all),
            cores: cores.collect(),
        }
    }
}

/// Reads the `cpu` lines of `/proc/stat`, e.g. `cpu0 39227 4 13880 3938621 3844 9517 9812 0 0 0`:
/// user, nice, system, idle, iowait, irq, softirq and steal, then guest and guest_nice, which
/// user and nice count already.
fn parse(text: &str) -> Option<Cpu> {
    let mut all = None;
    let mut cores = Vec::new();
    for line in text.lines() {
        let mut fields = line.split_whitespace();
        let Some(name) = fields.next().and_then(|name| name.strip_prefix("cpu")) else {
            continue;
        };
        let ticks: Vec<u64> = fields
            .take(8)
            .map_while(|field| field.parse().ok())
            .collect();
        if ticks.len() < 4 {
            continue;
        }
        let total = ticks.iter().sum();
        // Idle, and waiting for the disk with nothing else to do.
        let idle = ticks[3] + ticks.get(4).copied().unwrap_or(0);
        let times = Times {
            busy: total - idle,
            total,
        };
        if name.is_empty() {
            all = Some(times);
        } else if let Ok(n) = name.parse() {
            cores.push((n, times));
        }
    }
    Some(Cpu { all: all?, cores })
}

#[cfg(test)]
mod tests {
    use super::*;

    const STAT: &str = "\
cpu  392787 98 159935 63642586 41798 59944 28177 0 0 0
cpu0 39227 4 13880 3938621 3844 9517 9812 0 0 0
cpu1 19710 8 7580 3981780 1876 1994 2581 0 0 0
intr 1234 0 0
ctxt 5678
";

    #[test]
    fn reads_the_time_each_cpu_spent() {
        let cpu = parse(STAT).unwrap();
        let busy = 392_787 + 98 + 159_935 + 59_944 + 28_177;
        assert_eq!(cpu.all.busy, busy);
        assert_eq!(cpu.all.total, busy + 63_642_586 + 41_798);
        assert_eq!(cpu.cores.len(), 2);
        assert_eq!(cpu.cores[1].0, 1);
        assert_eq!(cpu.cores[1].1.busy, 19_710 + 8 + 7_580 + 1_994 + 2_581);
        assert_eq!(parse("intr 1234\n"), None);
    }

    #[test]
    fn tells_the_time_spent_between_two_readings() {
        let earlier = parse(STAT).unwrap();
        let later = parse(
            "cpu  392887 98 159935 63642686 41798 59944 28177 0 0 0\n\
             cpu0 39327 4 13880 3938621 3844 9517 9812 0 0 0\n\
             cpu2 1 0 0 1 0 0 0 0 0 0\n",
        )
        .unwrap();
        let spent = later.since(&earlier);
        let times = |busy, total| Times { busy, total };
        assert_eq!(spent.all, times(100, 200));
        // cpu2 came online meanwhile: it has nothing to compare with.
        assert_eq!(spent.cores, [(0, times(100, 100))]);
    }

    #[test]
    fn reads_this_system() {
        let cpu = Cpu::read().unwrap();
        assert!(!cpu.cores.is_empty());
        assert!(cpu.all.busy <= cpu.all.total);
    }
}
