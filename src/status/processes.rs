//! The processes, from `/proc/<pid>/stat`.

use std::collections::HashMap;
use std::fs;

/// A process: the CPU time and memory it takes.
#[derive(Debug, Clone, PartialEq)]
pub struct Process {
    pub pid: u32,
    /// Its name, as the kernel keeps it: the program's, cut to 15 bytes.
    pub name: String,
    /// Its share of one CPU between two readings, in percent.
    pub cpu: f64,
    /// The memory it holds, in bytes.
    pub memory: u64,
    /// The CPU time it took since it started, in clock ticks.
    ticks: u64,
    /// When it started, in clock ticks after the system did: a later process may get its pid.
    started: u64,
}

impl Process {
    #[cfg(test)]
    pub fn new(pid: u32, name: &str, cpu: f64, memory: u64) -> Self {
        Self {
            pid,
            name: name.into(),
            cpu,
            memory,
            ticks: 0,
            started: 0,
        }
    }
}

/// Every process there is now.
pub fn processes() -> Vec<Process> {
    let page = rustix::param::page_size() as u64;
    let pids = fs::read_dir("/proc").into_iter().flatten().flatten();
    pids.filter_map(|entry| {
        let pid = entry.file_name().to_str()?.parse().ok()?;
        let stat = fs::read_to_string(entry.path().join("stat")).ok()?;
        parse(pid, &stat, page)
    })
    .collect()
}

/// What each process in `now` took of one CPU between `earlier` and `now`, while each CPU spent
/// `ticks` clock ticks; with what it holds now.
pub fn rates(now: &[Process], earlier: &[Process], ticks: u64) -> Vec<Process> {
    let earlier: HashMap<(u32, u64), u64> = earlier
        .iter()
        .map(|process| ((process.pid, process.started), process.ticks))
        .collect();
    let rates = now.iter().map(|process| {
        // A process that started since took all its time in between.
        let before = earlier.get(&(process.pid, process.started)).copied();
        let spent = process.ticks.saturating_sub(before.unwrap_or(0));
        let cpu = if ticks == 0 {
            0.0
        } else {
            spent as f64 * 100.0 / ticks as f64
        };
        Process {
            cpu,
            ..process.clone()
        }
    });
    rates.collect()
}

/// Reads a `/proc/<pid>/stat` line, e.g. `1234 (Web Content) S 1 …`: the name is between the
/// first `(` and the last `)`, since it may hold either; then the user and system times are the
/// 14th and 15th fields, the start time the 22nd, and the pages held the 24th.
fn parse(pid: u32, stat: &str, page: u64) -> Option<Process> {
    let name = stat.get(stat.find('(')? + 1..stat.rfind(')')?)?;
    let fields: Vec<&str> = stat[stat.rfind(')')? + 1..].split_whitespace().collect();
    // The state is the 3rd field, the first after the name.
    let field = |n: usize| fields.get(n - 3)?.parse::<u64>().ok();
    Some(Process {
        pid,
        name: name.to_string(),
        cpu: 0.0,
        memory: field(24)? * page,
        ticks: field(14)? + field(15)?,
        started: field(22)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const STAT: &str = "4321 (Web (Content)) S 1 4321 4321 0 -1 4194560 1036548 0 1 0 \
                        12345 2345 0 0 20 0 31 0 98765 3871633408 52372 18446744073709551615";

    #[test]
    fn reads_a_process() {
        let process = parse(4321, STAT, 4096).unwrap();
        assert_eq!(process.name, "Web (Content)");
        assert_eq!(process.ticks, 14_690);
        assert_eq!(process.memory, 52_372 * 4096);
        assert_eq!(process.started, 98_765);
        assert_eq!(parse(1, "1 (init) S 1", 4096), None);
    }

    #[test]
    fn takes_a_share_of_one_cpu() {
        let process = |pid, ticks, started| Process {
            ticks,
            started,
            ..Process::new(pid, "p", 0.0, 0)
        };
        let earlier = [process(1, 100, 5), process(2, 100, 5)];
        // pid 2 is a process that started since.
        let now = [process(1, 150, 5), process(2, 30, 9), process(3, 10, 9)];
        let cpu: Vec<f64> = rates(&now, &earlier, 200)
            .iter()
            .map(|process| process.cpu)
            .collect();
        assert_eq!(cpu, [25.0, 15.0, 5.0]);
    }

    #[test]
    fn reads_this_system() {
        let own = std::process::id();
        assert!(processes().iter().any(|process| process.pid == own));
    }
}
