//! Temperatures, from the sensor chips in `/sys/class/hwmon`.

use std::fs;
use std::path::Path;

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

/// The temperature each sensor chip measures first, by chip.
pub fn temperatures() -> Vec<Temperature> {
    read_all(Path::new("/sys/class/hwmon"))
}

/// The first temperature of each chip in `hwmon`.
fn read_all(hwmon: &Path) -> Vec<Temperature> {
    let mut chips: Vec<_> = fs::read_dir(hwmon)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .collect();
    chips.sort();
    let mut temperatures: Vec<Temperature> = chips.iter().filter_map(|chip| first(chip)).collect();
    temperatures.sort_by(|a, b| a.chip.cmp(&b.chip));
    temperatures
}

/// The lowest-numbered temperature of the chip whose folder is `chip`, e.g. `temp1_input`, in
/// thousandths of a degree.
fn first(chip: &Path) -> Option<Temperature> {
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
    let read = |what: &str| fs::read_to_string(chip.join(format!("temp{number}_{what}")));
    let degrees = |what: &str| {
        let thousandths: f64 = read(what).ok()?.trim().parse().ok()?;
        Some(thousandths / 1000.0)
    };
    // Some chips give a limit no chip could reach, like 65261 °C.
    let limit = |what: &str| degrees(what).filter(|&limit| limit > 0.0 && limit < 200.0);
    let label = read("label").ok().map(|label| label.trim().to_string());
    Some(Temperature {
        chip: fs::read_to_string(chip.join("name"))
            .ok()?
            .trim()
            .to_string(),
        label: label.filter(|label| !label.is_empty()),
        celsius: degrees("input")?,
        high: limit("max").or_else(|| limit("crit")),
    })
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
        let temperatures = read_all(hwmon.path());
        let chips: Vec<&str> = temperatures.iter().map(|t| t.chip.as_str()).collect();
        assert_eq!(chips, ["acpitz", "coretemp", "nvme"]);
        let coretemp = &temperatures[1];
        assert_eq!(coretemp.label.as_deref(), Some("Package id 0"));
        assert_eq!(coretemp.celsius, 44.0);
        assert_eq!(coretemp.high, Some(100.0));
        assert!(!coretemp.is_high());
        let nvme = &temperatures[2];
        assert_eq!((nvme.celsius, nvme.high), (36.85, None));
    }

    #[test]
    fn reads_this_system() {
        for temperature in temperatures() {
            assert!(!temperature.chip.is_empty(), "{temperature:?}");
        }
    }
}
