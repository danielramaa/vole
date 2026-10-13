//! Batteries, from `/sys/class/power_supply`.

use std::fs;
use std::path::Path;
use std::time::Duration;

/// A battery of the computer, as it is now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Battery {
    /// What the kernel calls it, e.g. `BAT0`.
    pub name: String,
    /// How full it is, in percent.
    pub percent: u64,
    pub state: State,
    /// How long until it is empty or, charging, full; when the battery says what it draws.
    pub left: Option<Duration>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Charging,
    Discharging,
    Full,
    /// Plugged in, but kept from charging, as some laptops do below a set level.
    NotCharging,
    Unknown,
}

/// The most hours a battery is said to last or take to charge.
const LONGEST: f64 = 99.0;

/// The computer's batteries; a wireless mouse's or keyboard's aren't.
pub fn batteries() -> Vec<Battery> {
    read_all(Path::new("/sys/class/power_supply"))
}

fn read_all(power_supply: &Path) -> Vec<Battery> {
    let mut batteries: Vec<Battery> = fs::read_dir(power_supply)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| read(&entry.path()))
        .collect();
    batteries.sort_by(|a, b| a.name.cmp(&b.name));
    batteries
}

fn read(supply: &Path) -> Option<Battery> {
    let field = |name: &str| fs::read_to_string(supply.join(name)).ok();
    let number = |name: &str| field(name)?.trim().parse::<u64>().ok();
    if field("type")?.trim() != "Battery" || field("scope").is_some_and(|s| s.trim() == "Device") {
        return None;
    }
    let state = match field("status").as_deref().map(str::trim) {
        Some("Charging") => State::Charging,
        Some("Discharging") => State::Discharging,
        Some("Full") => State::Full,
        Some("Not charging") => State::NotCharging,
        _ => State::Unknown,
    };
    // In µWh and µW, or µAh and µA, as the battery counts.
    let (now, full, draw) = match number("energy_now") {
        Some(now) => (Some(now), number("energy_full"), number("power_now")),
        None => (
            number("charge_now"),
            number("charge_full"),
            number("current_now"),
        ),
    };
    let percent = number("capacity").or_else(|| Some(now? * 100 / full.filter(|&f| f > 0)?))?;
    let hours = || {
        let (now, draw) = (now? as f64, draw.filter(|&draw| draw > 0)? as f64);
        match state {
            State::Discharging => Some(now / draw),
            State::Charging => Some((full? as f64 - now).max(0.0) / draw),
            State::Full | State::NotCharging | State::Unknown => None,
        }
    };
    let name = supply.file_name()?.to_string_lossy().into_owned();
    Some(Battery {
        name,
        percent: percent.min(100),
        state,
        // Past that, it draws too little to tell, as some batteries say just after unplugging.
        left: hours()
            .filter(|&hours| hours < LONGEST)
            .map(|hours| Duration::from_secs((hours * 3600.0).round() as u64)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testdir::TestDir;

    /// A power supply in `folder` with `files` such as `("type", "Battery")`.
    fn supply(power_supply: &TestDir, folder: &str, files: &[(&str, &str)]) {
        let supply = power_supply.dir(folder);
        for (file, value) in files {
            fs::write(supply.join(file), format!("{value}\n")).unwrap();
        }
    }

    #[test]
    fn reads_the_computer_s_batteries() {
        let power_supply = TestDir::new();
        supply(
            &power_supply,
            "BAT0",
            &[
                ("type", "Battery"),
                ("status", "Discharging"),
                ("capacity", "56"),
                ("charge_now", "4533000"),
                ("charge_full", "8096000"),
                ("current_now", "1236000"),
            ],
        );
        supply(&power_supply, "ADP1", &[("type", "Mains"), ("online", "0")]);
        supply(
            &power_supply,
            "hidpp_battery_0",
            &[("type", "Battery"), ("scope", "Device"), ("capacity", "80")],
        );
        let batteries = read_all(power_supply.path());
        assert_eq!(batteries.len(), 1);
        let battery = &batteries[0];
        assert_eq!((battery.name.as_str(), battery.percent), ("BAT0", 56));
        assert_eq!(battery.state, State::Discharging);
        // 4533 mAh at 1236 mA.
        assert_eq!(battery.left, Some(Duration::from_secs(13_203)));
    }

    #[test]
    fn tells_how_long_until_full_from_energy() {
        let power_supply = TestDir::new();
        supply(
            &power_supply,
            "BAT1",
            &[
                ("type", "Battery"),
                ("status", "Charging"),
                ("energy_now", "30000000"),
                ("energy_full", "50000000"),
                ("power_now", "10000000"),
            ],
        );
        supply(
            &power_supply,
            "BAT0",
            &[("type", "Battery"), ("status", "Full"), ("capacity", "100")],
        );
        supply(
            &power_supply,
            "BAT2",
            &[
                ("type", "Battery"),
                ("status", "Discharging"),
                ("capacity", "80"),
                ("energy_now", "40000000"),
                ("power_now", "1000"),
            ],
        );
        supply(
            &power_supply,
            "BAT3",
            &[
                ("type", "Battery"),
                ("status", "Charging"),
                ("capacity", "99"),
            ],
        );
        let batteries = read_all(power_supply.path());
        assert_eq!(batteries.len(), 4);
        // Drawing 1 mW, it would last 40000 hours: too little to tell.
        assert_eq!((batteries[2].percent, batteries[2].left), (80, None));
        // Saying neither how full it is in µWh nor what it draws, it still shows.
        assert_eq!(
            (batteries[3].state, batteries[3].left),
            (State::Charging, None)
        );
        assert_eq!(batteries[0].state, State::Full);
        assert_eq!(batteries[0].left, None);
        assert_eq!(batteries[1].percent, 60);
        assert_eq!(batteries[1].left, Some(Duration::from_secs(2 * 3600)));
    }
}
