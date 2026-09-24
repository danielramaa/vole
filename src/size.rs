//! Byte sizes: shown to the user and read from the output of official tools.

const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];

pub const MIB: u64 = 1024 * 1024;

/// Formats a size for display, e.g. `512 B` or `1.4 GiB`.
pub fn format(bytes: u64) -> String {
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// Parses a size such as `164.84 MiB` (paccache) or `1.2G` (journalctl). Units are powers of 1024.
pub fn parse(text: &str) -> Option<u64> {
    let text = text.trim();
    let split = text
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(text.len());
    let (number, unit) = text.split_at(split);
    let number: f64 = number.parse().ok()?;
    let power = match unit.trim() {
        "" | "B" => 0,
        "K" | "KiB" => 1,
        "M" | "MiB" => 2,
        "G" | "GiB" => 3,
        "T" | "TiB" => 4,
        _ => return None,
    };
    Some((number * 1024f64.powi(power)).round() as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_sizes() {
        assert_eq!(format(0), "0 B");
        assert_eq!(format(1023), "1023 B");
        assert_eq!(format(1536), "1.5 KiB");
        assert_eq!(format(3 * 1024 * MIB), "3.0 GiB");
    }

    #[test]
    fn parses_tool_output_sizes() {
        assert_eq!(parse("512B"), Some(512));
        assert_eq!(parse("8.0K"), Some(8192));
        assert_eq!(parse("1.5G"), Some(1536 * MIB));
        assert_eq!(
            parse("164.84 MiB"),
            Some((164.84 * MIB as f64).round() as u64)
        );
        assert_eq!(parse("12 parsecs"), None);
        assert_eq!(parse("MiB"), None);
    }
}
