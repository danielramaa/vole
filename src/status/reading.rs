//! Readings of the system, taken on a thread of their own when the screen asks for one, so a file
//! of `/sys` that is slow to read never holds the screen up.

use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use super::cpu::Cpu;
use super::disks::{self, DiskIo, Filesystem};
use super::memory::Memory;
use super::network::{self, Traffic};
use super::sensors::{self, Temperature};

/// Readings further apart than this tell nothing about now: the dashboard wasn't on screen in
/// between. The next one starts afresh.
const GAP: Duration = Duration::from_secs(10);

/// What the dashboard shows, read at one moment.
#[derive(Debug, Default)]
pub struct Reading {
    /// The time the CPUs spent since the reading before; `None` without one close enough before.
    pub cpu: Option<Cpu>,
    pub memory: Option<Memory>,
    pub filesystems: Vec<Filesystem>,
    /// What each disk read and wrote each second since the reading before.
    pub disk_io: Option<Vec<DiskIo>>,
    /// What each interface received and sent each second since the reading before.
    pub network: Option<Vec<Traffic>>,
    pub temperatures: Vec<Temperature>,
}

/// The thread taking readings. It ends with its `Reader`.
pub struct Reader {
    ask: Sender<()>,
    readings: Receiver<Reading>,
}

impl Reader {
    pub fn start() -> Self {
        let (ask, asked) = mpsc::channel();
        let (sender, readings) = mpsc::channel();
        thread::spawn(move || {
            let mut before = None;
            for () in asked {
                // The screen may have gone already; then nobody is listening.
                if sender.send(read(&mut before, Instant::now())).is_err() {
                    break;
                }
            }
        });
        Self { ask, readings }
    }

    /// Asks for a reading, which [`Reader::take`] gets once it has been taken.
    pub fn ask(&self) {
        let _ = self.ask.send(());
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
}

/// Takes a reading `at` now, telling what changed since the reading `before`, which it then
/// replaces.
fn read(before: &mut Option<Counters>, at: Instant) -> Reading {
    let now = Counters {
        at,
        cpu: Cpu::read(),
        disk_io: disks::io(),
        network: network::traffic(),
    };
    let mut reading = Reading {
        memory: Memory::read(),
        filesystems: disks::filesystems(),
        temperatures: sensors::temperatures(),
        ..Reading::default()
    };
    let earlier = before
        .take()
        .filter(|earlier| at.duration_since(earlier.at) <= GAP);
    if let Some(earlier) = earlier {
        let elapsed = at.duration_since(earlier.at);
        reading.cpu = (now.cpu.as_ref())
            .zip(earlier.cpu.as_ref())
            .map(|(now, earlier)| now.since(earlier));
        reading.disk_io = Some(DiskIo::rates(&now.disk_io, &earlier.disk_io, elapsed));
        reading.network = Some(Traffic::rates(&now.network, &earlier.network, elapsed));
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
    fn rates_need_a_reading_shortly_before() {
        let start = Instant::now();
        let mut before = None;
        assert!(read(&mut before, start).cpu.is_none());
        assert!(
            read(&mut before, start + Duration::from_secs(1))
                .cpu
                .is_some()
        );
        // Long after, as when the dashboard was left and opened again.
        assert!(read(&mut before, start + GAP * 3).cpu.is_none());
    }

    #[test]
    fn counts_grow_per_second() {
        let half = Duration::from_millis(500);
        assert_eq!(per_second(3000, 1000, half), 4000);
        // A count that went back, as when a disk was swapped for another of the same name.
        assert_eq!(per_second(1000, 3000, half), 0);
        assert_eq!(per_second(3000, 1000, Duration::ZERO), 0);
    }

    #[test]
    fn readings_come_when_asked() {
        let reader = Reader::start();
        reader.ask();
        let reading = loop {
            if let Some(reading) = reader.take().unwrap() {
                break reading;
            }
            thread::sleep(Duration::from_millis(5));
        };
        assert!(reading.cpu.is_none());
        assert!(reader.take().unwrap().is_none());
    }
}
