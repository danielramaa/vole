//! Readings of the system, taken on a thread of their own when the screen asks for one, so a file
//! of `/sys` that is slow to read never holds the screen up.

use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;
use std::time::{Duration, Instant};

use super::cpu::Cpu;
use super::memory::Memory;

/// Readings further apart than this tell nothing about now: the dashboard wasn't on screen in
/// between. The next one starts afresh.
const GAP: Duration = Duration::from_secs(10);

/// What the dashboard shows, read at one moment.
#[derive(Debug, Default)]
pub struct Reading {
    /// The time the CPUs spent since the reading before; `None` without one close enough before.
    pub cpu: Option<Cpu>,
    pub memory: Option<Memory>,
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
}

/// Takes a reading `at` now, telling what changed since the reading `before`, which it then
/// replaces.
fn read(before: &mut Option<Counters>, at: Instant) -> Reading {
    let now = Counters {
        at,
        cpu: Cpu::read(),
    };
    let earlier = before
        .take()
        .filter(|earlier| at.duration_since(earlier.at) <= GAP);
    let earlier = earlier.as_ref();
    let cpu = match (&now.cpu, earlier.and_then(|earlier| earlier.cpu.as_ref())) {
        (Some(now), Some(earlier)) => Some(now.since(earlier)),
        _ => None,
    };
    *before = Some(now);
    Reading {
        cpu,
        memory: Memory::read(),
    }
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
