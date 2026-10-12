//! Temperatures, from the sensor chips in `/sys/class/hwmon`.

use std::fs;
use std::path::{Path, PathBuf};

/// What a sensor chip measures first: for a CPU, the whole package; for an NVMe disk, its
/// overall temperature.
#[derive(Debug, Clone, PartialEq)]
pub struct Temperature {
    /// The chip, e.g. `coretemp` or `nvme`.
    pub chip: String,
    /// What the reading is of, e.g. `Package id 0`, when the chip says.
    pub label: Option<String>,
    pub celsius: f64,
    /// How hot it may get before it is too hot, when the chip says.
    pub high: Option<f64>,
}

impl Temperature {
    pub fn is_high(&self) -> bool {
        self.high.is_some_and(|high| self.celsius >= high)
    }
}

/// The sensor chips, with what doesn't change about their first temperature: what it is of, and
/// how hot it may get. Asking a chip for those can take long, as an NVMe disk answers each
/// question in turn, so they are asked once, and only the temperature each time.
#[derive(Debug, Default)]
pub struct Sensors {
    /// The chips' folders when they were last looked at, to tell when one comes or goes.
    folders: Vec<PathBuf>,
    /// Each chip's first temperature, by chip, with the file it is read from.
    chips: Vec<(Temperature, PathBuf)>,
}

impl Sensors {
    /// The temperature each sensor chip measures first now, by chip.
    pub fn read(&mut self) -> Vec<Temperature> {
        self.read_in(Path::new("/sys/class/hwmon"))
    }

    /// [`Sensors::read`], with the chips in `hwmon`.
    fn read_in(&mut self, hwmon: &Path) -> Vec<Temperature> {
        let mut folders: Vec<PathBuf> = fs::read_dir(hwmon)
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path())
            .collect();
        folders.sort();
        if folders != self.folders {
            self.chips = folders.iter().filter_map(|chip| first(chip)).collect();
            self.chips.sort_by(|(a, _), (b, _)| a.chip.cmp(&b.chip));
            self.folders = folders;
        }
        let read = self.chips.iter().filter_map(|(temperature, input)| {
            Some(Temperature {
                celsius: degrees(input)?,
                ..temperature.clone()
            })
        });
        read.collect()
    }
}

/// The lowest-numbered temperature of the chip whose folder is `chip`, e.g. `temp1_input`, and
/// the file it is read from.
fn first(chip: &Path) -> Option<(Temperature, PathBuf)> {
    let number = fs::read_dir(chip)
        .ok()?
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name();
            let number = name
                .to_str()?
                .strip_prefix("temp")?
                .strip_suffix("_input")?;
            number.parse::<u32>().ok()
        })
        .min()?;
    let file = |what: &str| chip.join(format!("temp{number}_{what}"));
    // Some chips give a limit no chip could reach, like 65261 °C.
    let limit = |what: &str| degrees(&file(what)).filter(|&limit| limit > 0.0 && limit < 200.0);
    let label = fs::read_to_string(file("label")).ok();
    let temperature = Temperature {
        chip: fs::read_to_string(chip.join("name"))
            .ok()?
            .trim()
            .to_string(),
        label: label
            .map(|label| label.trim().to_string())
            .filter(|label| !label.is_empty()),
        celsius: 0.0,
        high: limit("max").or_else(|| limit("crit")),
    };
    Some((temperature, file("input")))
}

/// The degrees in `file`, which says them in thousandths.
fn degrees(file: &Path) -> Option<f64> {
    let thousandths: f64 = fs::read_to_string(file).ok()?.trim().parse().ok()?;
    Some(thousandths / 1000.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testdir::TestDir;

    /// A chip in `hwmon` named `name`, with `files` such as `("temp1_input", "44000")`.
    fn chip(hwmon: &TestDir, folder: &str, name: &str, files: &[(&str, &str)]) {
        let chip = hwmon.dir(folder);
        fs::write(chip.join("name"), format!("{name}\n")).unwrap();
        for (file, value) in files {
            fs::write(chip.join(file), format!("{value}\n")).unwrap();
        }
    }

    #[test]
    fn reads_the_first_temperature_of_each_chip() {
        let hwmon = TestDir::new();
        chip(
            &hwmon,
            "hwmon7",
            "coretemp",
            &[
                ("temp1_input", "44000"),
                ("temp1_label", "Package id 0"),
                ("temp1_max", "100000"),
                ("temp2_input", "39000"),
                ("temp10_input", "51000"),
            ],
        );
        chip(
            &hwmon,
            "hwmon4",
            "nvme",
            &[
                ("temp2_input", "36850"),
                ("temp2_max", "65261850"),
                ("temp3_input", "30000"),
            ],
        );
        chip(&hwmon, "hwmon2", "acpitz", &[("temp1_input", "27800")]);
        chip(&hwmon, "hwmon0", "ADP1", &[]);
        let mut sensors = Sensors::default();
        let temperatures = sensors.read_in(hwmon.path());
        let chips: Vec<&str> = temperatures.iter().map(|t| t.chip.as_str()).collect();
        assert_eq!(chips, ["acpitz", "coretemp", "nvme"]);
        let coretemp = &temperatures[1];
        assert_eq!(coretemp.label.as_deref(), Some("Package id 0"));
        assert_eq!(coretemp.celsius, 44.0);
        assert_eq!(coretemp.high, Some(100.0));
        assert!(!coretemp.is_high());
        let nvme = &temperatures[2];
        assert_eq!((nvme.celsius, nvme.high), (36.85, None));
        // Only the temperatures are read again, until a chip comes or goes.
        fs::write(hwmon.path().join("hwmon7/temp1_input"), "45000\n").unwrap();
        fs::write(hwmon.path().join("hwmon7/temp1_label"), "Changed\n").unwrap();
        let coretemp = &sensors.read_in(hwmon.path())[1];
        assert_eq!(coretemp.celsius, 45.0);
        assert_eq!(coretemp.label.as_deref(), Some("Package id 0"));
        chip(&hwmon, "hwmon8", "k10temp", &[("temp1_input", "50000")]);
        let temperatures = sensors.read_in(hwmon.path());
        assert_eq!(temperatures.len(), 4);
        assert_eq!(temperatures[1].label.as_deref(), Some("Changed"));
    }

    #[test]
    fn reads_this_system() {
        for temperature in Sensors::default().read() {
            assert!(!temperature.chip.is_empty(), "{temperature:?}");
        }
    }
}
