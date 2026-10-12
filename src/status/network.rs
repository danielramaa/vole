//! The network: what the interfaces received and sent, from `/proc/net/dev`.

use std::fs;
use std::path::Path;
use std::time::Duration;

use super::reading::per_second;

/// Bytes an interface received and sent: since it came up, or each second between two readings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Traffic {
    pub name: String,
    pub received: u64,
    pub sent: u64,
}

impl Traffic {
    /// What each interface in both `now` and `earlier` received and sent each second in between,
    /// `elapsed` apart.
    pub fn rates(now: &[Traffic], earlier: &[Traffic], elapsed: Duration) -> Vec<Traffic> {
        let rates = now.iter().filter_map(|interface| {
            let before = earlier
                .iter()
                .find(|before| before.name == interface.name)?;
            Some(Traffic {
                name: interface.name.clone(),
                received: per_second(interface.received, before.received, elapsed),
                sent: per_second(interface.sent, before.sent, elapsed),
            })
        });
        rates.collect()
    }
}

/// What the interfaces that are up received and sent since they came up: the ones leading out of
/// the computer, since what goes through bridges, VPNs and containers' interfaces goes through
/// one of them too. In a container, which has only those, all of them but loopback.
pub fn traffic() -> Vec<Traffic> {
    let Ok(dev) = fs::read_to_string("/proc/net/dev") else {
        return Vec::new();
    };
    leading_out(Path::new("/sys"), parse(&dev))
}

/// The interfaces of `all` that are up and lead out of the computer, as `sys` tells; or, when
/// none does, all that are up but loopback.
fn leading_out(sys: &Path, all: Vec<Traffic>) -> Vec<Traffic> {
    let up: Vec<Traffic> = all
        .into_iter()
        .filter(|interface| interface.name != "lo" && is_up(sys, &interface.name))
        .collect();
    let out: Vec<Traffic> = up
        .iter()
        .filter(|interface| !is_virtual(sys, &interface.name))
        .cloned()
        .collect();
    if out.is_empty() { up } else { out }
}

/// Reads `/proc/net/dev` lines, e.g. `  wlan0: 2854366736 2268464 0 0 0 0 0 0 102366473 653750 …`:
/// the bytes received come first after the name, and the bytes sent 9th.
fn parse(text: &str) -> Vec<Traffic> {
    let interface = |line: &str| {
        let (name, counts) = line.split_once(':')?;
        let counts: Vec<&str> = counts.split_whitespace().collect();
        let count = |n: usize| counts.get(n)?.parse::<u64>().ok();
        Some(Traffic {
            name: name.trim().to_string(),
            received: count(0)?,
            sent: count(8)?,
        })
    };
    text.lines().filter_map(interface).collect()
}

fn is_up(sys: &Path, name: &str) -> bool {
    let state = fs::read_to_string(sys.join("class/net").join(name).join("operstate"));
    // Some drivers never say, though the interface works.
    state.is_ok_and(|state| matches!(state.trim(), "up" | "unknown"))
}

/// Whether the kernel made the interface up: a bridge, a VPN's, a container's, loopback.
fn is_virtual(sys: &Path, name: &str) -> bool {
    fs::canonicalize(sys.join("class/net").join(name))
        .is_ok_and(|device| device.starts_with(sys.join("devices/virtual")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testdir::TestDir;

    const DEV: &str = "\
Inter-|   Receive                                                |  Transmit
 face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed
    lo:  167166    1881    0    0    0     0          0         0   167166    1881    0    0    0     0       0          0
 wlan0:2854366736 2268464    0    0    0     0          0         0 102366473  653750    0   16    0     0       0          0
docker0:       0       0    0    0    0     0          0         0        0       0    0    2    0     0       0          0
";

    fn traffic(name: &str, received: u64, sent: u64) -> Traffic {
        Traffic {
            name: name.into(),
            received,
            sent,
        }
    }

    #[test]
    fn reads_what_each_interface_received_and_sent() {
        assert_eq!(
            parse(DEV),
            [
                traffic("lo", 167_166, 167_166),
                traffic("wlan0", 2_854_366_736, 102_366_473),
                traffic("docker0", 0, 0),
            ]
        );
    }

    /// `sys` with an interface `name` in `devices/<place>`, in `state`.
    fn interface(sys: &TestDir, name: &str, place: &str, state: &str) {
        let device = sys.dir(&format!("devices/{place}/{name}"));
        fs::write(device.join("operstate"), format!("{state}\n")).unwrap();
        sys.symlink(&format!("class/net/{name}"), &device);
    }

    #[test]
    fn counts_the_interfaces_leading_out() {
        let sys = TestDir::new();
        interface(&sys, "wlan0", "pci0000:00/net", "up");
        interface(&sys, "enp3s0", "pci0000:00/net", "down");
        interface(&sys, "docker0", "virtual/net", "up");
        interface(&sys, "lo", "virtual/net", "unknown");
        let names = |interfaces: Vec<Traffic>| -> Vec<String> {
            interfaces
                .into_iter()
                .map(|interface| interface.name)
                .collect()
        };
        let all = || {
            ["lo", "wlan0", "enp3s0", "docker0"]
                .map(|name| traffic(name, 1, 1))
                .to_vec()
        };
        assert_eq!(names(leading_out(sys.path(), all())), ["wlan0"]);
        // In a container, only virtual interfaces.
        let all = all().into_iter().filter(|i| i.name != "wlan0").collect();
        assert_eq!(names(leading_out(sys.path(), all)), ["docker0"]);
    }

    #[test]
    fn rates_are_per_second() {
        let rates = Traffic::rates(
            &[traffic("wlan0", 3000, 1000)],
            &[traffic("wlan0", 1000, 1000), traffic("eth0", 0, 0)],
            Duration::from_secs(2),
        );
        assert_eq!(rates, [traffic("wlan0", 1000, 0)]);
    }
}
