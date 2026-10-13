//! Readings of the system, taken on a thread of their own when the screen asks for one, so a file
//! of `/sys` that is slow to read never holds the screen up.

use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use super::battery::{self, Battery};
use super::cpu::Cpu;
use super::disks::{self, DiskIo, Filesystem};
use super::memory::Memory;
use super::network::{self, Traffic};
use super::processes::{self, Process};
use super::sensors::{Sensors, Temperature};

/// How long apart the two readings are that the first rates are told from.
const FIRST: Duration = Duration::from_millis(250);

/// What the dashboard shows, read at one moment.
#[derive(Debug, Default)]
pub struct Reading {
    /// How long since the reading before; `None` without one.
    pub elapsed: Option<Duration>,
    /// The time the CPUs spent since the reading before.
    pub cpu: Option<Cpu>,
    pub memory: Option<Memory>,
    pub filesystems: Vec<Filesystem>,
    /// What each disk read and wrote each second since the reading before.
    pub disk_io: Option<Vec<DiskIo>>,
    /// What each interface received and sent each second since the reading before.
    pub network: Option<Vec<Traffic>>,
    pub temperatures: Vec<Temperature>,
    pub batteries: Vec<Battery>,
    /// Every process, with what it took of one CPU since the reading before.
    pub processes: Option<Vec<Process>>,
}

/// The thread taking readings. It ends with its `Reader`.
pub struct Reader {
    /// Asks for a reading; `true` to start afresh.
    ask: Sender<bool>,
    readings: Receiver<Reading>,
}

impl Reader {
    pub fn start() -> Self {
        let (ask, asked) = mpsc::channel();
        let (sender, readings) = mpsc::channel();
        thread::spawn(move || {
            let mut before = None;
            let mut sensors = Sensors::default();
            for afresh in asked {
                if afresh {
                    before = None;
                }
                // Rates are told from the reading before: without one, one now, and the one
                // asked for shortly after.
                if before.is_none() {
                    read(&mut before, &mut sensors, Instant::now());
                    thread::sleep(FIRST);
                }
                let reading = read(&mut before, &mut sensors, Instant::now());
                // The screen may have gone already; then nobody is listening.
                if sender.send(reading).is_err() {
                    break;
                }
            }
        });
        Self { ask, readings }
    }

    /// Asks for a reading, which [`Reader::take`] gets once it has been taken; `afresh` when the
    /// readings before say nothing about now, as when the dashboard wasn't on screen since.
    pub fn ask(&self, afresh: bool) {
        let _ = self.ask.send(afresh);
    }

    /// The reading asked for, once it is there; an error when the thread stopped.
    pub fn take(&self) -> Result<Option<Reading>, TryRecvError> {
        match self.readings.try_recv() {
            Ok(reading) => Ok(Some(reading)),
            Err(TryRecvError::Empty) => Ok(None),
            Err(err) => Err(err),
        }
    }
}

/// The counters of a reading, which the next one is told from.
struct Counters {
    at: Instant,
    cpu: Option<Cpu>,
    disk_io: Vec<DiskIo>,
    network: Vec<Traffic>,
    processes: Vec<Process>,
}

/// Takes a reading `at` now, telling what changed since the reading `before`, which it then
/// replaces, with the `sensors` found so far.
fn read(before: &mut Option<Counters>, sensors: &mut Sensors, at: Instant) -> Reading {
    let now = Counters {
        at,
        cpu: Cpu::read(),
        disk_io: disks::io(),
        network: network::traffic(),
        processes: processes::processes(),
    };
    let mut reading = Reading {
        memory: Memory::read(),
        filesystems: disks::filesystems(),
        temperatures: sensors.read(),
        batteries: battery::batteries(),
        ..Reading::default()
    };
    if let Some(earlier) = before.take() {
        let elapsed = at.duration_since(earlier.at);
        reading.elapsed = Some(elapsed);
        reading.cpu = (now.cpu.as_ref())
            .zip(earlier.cpu.as_ref())
            .map(|(now, earlier)| now.since(earlier));
        reading.disk_io = Some(DiskIo::rates(&now.disk_io, &earlier.disk_io, elapsed));
        reading.network = Some(Traffic::rates(&now.network, &earlier.network, elapsed));
        if let Some(cpu) = &reading.cpu {
            // The ticks each CPU spent in between, which a process's share is out of.
            let ticks = cpu.all.total / cpu.cores.len().max(1) as u64;
            let rates = processes::rates(&now.processes, &earlier.processes, ticks);
            reading.processes = Some(rates);
        }
    }
    *before = Some(now);
    reading
}

/// How much a count grew each second, from `earlier` to `now`, `elapsed` apart.
pub fn per_second(now: u64, earlier: u64, elapsed: Duration) -> u64 {
    let seconds = elapsed.as_secs_f64();
    if seconds <= 0.0 {
        return 0;
    }
    (now.saturating_sub(earlier) as f64 / seconds).round() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rates_need_a_reading_before() {
        let start = Instant::now();
        let (mut before, mut sensors) = (None, Sensors::default());
        let mut read_at = |at| read(&mut before, &mut sensors, at);
        assert!(read_at(start).cpu.is_none());
        let second = Duration::from_secs(1);
        let reading = read_at(start + second);
        assert!(reading.cpu.is_some() && reading.processes.is_some());
        assert_eq!(reading.elapsed, Some(second));
    }

    #[test]
    fn counts_grow_per_second() {
        let half = Duration::from_millis(500);
        assert_eq!(per_second(3000, 1000, half), 4000);
        // A count that went back, as when a disk was swapped for another of the same name.
        assert_eq!(per_second(1000, 3000, half), 0);
        assert_eq!(per_second(3000, 1000, Duration::ZERO), 0);
    }

    /// The reading `reader` was asked for, once it comes.
    fn next(reader: &Reader) -> Reading {
        loop {
            if let Some(reading) = reader.take().unwrap() {
                break reading;
            }
            thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn every_reading_has_rates_even_the_first() {
        let reader = Reader::start();
        reader.ask(false);
        let first = next(&reader);
        assert!(first.cpu.is_some());
        assert!(first.elapsed.is_some_and(|elapsed| elapsed >= FIRST));
        assert!(reader.take().unwrap().is_none());
        reader.ask(false);
        assert!(next(&reader).elapsed.is_some_and(|elapsed| elapsed < FIRST));
        // Afresh, as when the dashboard comes back: from a reading just taken.
        reader.ask(true);
        assert!(
            next(&reader)
                .elapsed
                .is_some_and(|elapsed| elapsed >= FIRST)
        );
    }
}
