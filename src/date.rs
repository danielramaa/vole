//! Dates without a calendar library, all in UTC: Backup folder names, and when things changed.

use std::time::{SystemTime, UNIX_EPOCH};

const DAY: i64 = 86_400;

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// A moment as a Backup folder name, e.g. `2026-10-05T13-30-00Z`: sorts by date, and has no
/// colons.
pub fn stamp(time: SystemTime) -> String {
    let secs = secs(time);
    let (year, month, day) = civil_from_days(secs.div_euclid(DAY));
    let s = secs.rem_euclid(DAY);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}-{:02}-{:02}Z",
        s / 3600,
        s / 60 % 60,
        s % 60
    )
}

/// E.g. `Mar 2025`.
pub fn month(time: SystemTime) -> String {
    let (year, month, _) = civil_from_days(secs(time).div_euclid(DAY));
    format!("{} {year}", MONTHS[(month - 1) as usize])
}

/// Whole days from `earlier` to `later`; 0 when `earlier` isn't earlier.
pub fn days_between(earlier: SystemTime, later: SystemTime) -> u64 {
    later
        .duration_since(earlier)
        .map_or(0, |d| d.as_secs() / DAY as u64)
}

/// Seconds since the Unix epoch, negative before it.
fn secs(time: SystemTime) -> i64 {
    match time.duration_since(UNIX_EPOCH) {
        Ok(after) => i64::try_from(after.as_secs()).unwrap_or(i64::MAX),
        Err(before) => -i64::try_from(before.duration().as_secs()).unwrap_or(i64::MAX),
    }
}

// Howard Hinnant's conversion from days since 1970-01-01 to (year, month, day) in the proleptic
// Gregorian calendar.

fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let days = days + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn at(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    #[test]
    fn stamps_name_the_moment() {
        assert_eq!(stamp(at(0)), "1970-01-01T00-00-00Z");
        assert_eq!(stamp(at(1_791_207_000)), "2026-10-05T13-30-00Z");
        assert_eq!(stamp(at(1_709_251_199)), "2024-02-29T23-59-59Z");
        assert_eq!(stamp(at(951_868_800)), "2000-03-01T00-00-00Z");
    }

    #[test]
    fn says_months_and_days() {
        assert_eq!(month(at(1_791_207_000)), "Oct 2026");
        assert_eq!(month(at(1_709_251_199)), "Feb 2024");
        assert_eq!(month(at(951_868_800)), "Mar 2000");
        assert_eq!(month(at(946_641_600)), "Dec 1999");
        assert_eq!(days_between(at(0), at(3 * 86_400 + 5)), 3);
        assert_eq!(days_between(at(10), at(0)), 0);
    }
}
