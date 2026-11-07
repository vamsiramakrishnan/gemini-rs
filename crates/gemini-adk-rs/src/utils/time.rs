//! Wall-clock timestamps without a date-time dependency.
//!
//! The runtime reads time through a [`Clock`](crate::clock::Clock) wherever a
//! decision depends on it. These helpers are for the other case: stamping a
//! record (a session, a trace, a stored artifact) with when it was written.

use std::time::{SystemTime, UNIX_EPOCH};

/// Seconds since the Unix epoch; `0` if the system clock is before it.
pub(crate) fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// The current time as RFC 3339 in UTC, to the second:
/// `2024-02-29T12:34:56Z`.
pub fn now_rfc3339() -> String {
    format_rfc3339(SystemTime::now())
}

/// The current time as RFC 3339 in UTC, to the millisecond:
/// `2024-02-29T12:34:56.789Z`.
pub fn now_rfc3339_millis() -> String {
    format_rfc3339_millis(SystemTime::now())
}

/// `time` as RFC 3339 in UTC, to the second (`2024-02-29T12:34:56Z`).
///
/// A time before the Unix epoch is formatted as the epoch.
///
/// ```
/// use std::time::{Duration, UNIX_EPOCH};
/// use gemini_adk_rs::utils::time::format_rfc3339;
///
/// assert_eq!(format_rfc3339(UNIX_EPOCH), "1970-01-01T00:00:00Z");
/// assert_eq!(
///     format_rfc3339(UNIX_EPOCH + Duration::from_secs(1_709_210_096)),
///     "2024-02-29T12:34:56Z"
/// );
/// ```
pub fn format_rfc3339(time: SystemTime) -> String {
    let since_epoch = time.duration_since(UNIX_EPOCH).unwrap_or_default();
    let (date, clock) = civil(since_epoch.as_secs());
    format!("{date}T{clock}Z")
}

/// `time` as RFC 3339 in UTC, to the millisecond (`2024-02-29T12:34:56.789Z`).
///
/// A time before the Unix epoch is formatted as the epoch.
pub fn format_rfc3339_millis(time: SystemTime) -> String {
    let since_epoch = time.duration_since(UNIX_EPOCH).unwrap_or_default();
    let (date, clock) = civil(since_epoch.as_secs());
    format!("{date}T{clock}.{:03}Z", since_epoch.subsec_millis())
}

/// `("YYYY-MM-DD", "HH:MM:SS")` for `secs` since the Unix epoch.
fn civil(secs: u64) -> (String, String) {
    let (days, rem) = (secs / 86_400, secs % 86_400);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    (
        format!("{year:04}-{month:02}-{day:02}"),
        format!("{:02}:{:02}:{:02}", rem / 3_600, rem % 3_600 / 60, rem % 60),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn at(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    #[test]
    fn known_instants_format_as_rfc_3339() {
        let cases = [
            (0, "1970-01-01T00:00:00Z"),
            (86_399, "1970-01-01T23:59:59Z"),
            (86_400, "1970-01-02T00:00:00Z"),
            // 2000 is a leap year (divisible by 400): Feb 29 exists.
            (951_782_400, "2000-02-29T00:00:00Z"),
            (951_868_800, "2000-03-01T00:00:00Z"),
            // 2024 leap day, mid-day.
            (1_709_210_096, "2024-02-29T12:34:56Z"),
            // Century rule: 2100 is not a leap year, so no Feb 29 there.
            (4_107_542_400, "2100-03-01T00:00:00Z"),
            (1_000_000_000, "2001-09-09T01:46:40Z"),
            (2_000_000_000, "2033-05-18T03:33:20Z"),
            (4_102_444_799, "2099-12-31T23:59:59Z"),
        ];
        for (secs, expected) in cases {
            assert_eq!(format_rfc3339(at(secs)), expected, "{secs}");
        }
    }

    #[test]
    fn leap_day_is_followed_by_march_first() {
        assert_eq!(format_rfc3339(at(1_709_164_800)), "2024-02-29T00:00:00Z");
        assert_eq!(
            format_rfc3339(at(1_709_164_800 + 86_400)),
            "2024-03-01T00:00:00Z"
        );
    }

    #[test]
    fn millis_variant_adds_three_digits() {
        let t = at(1_709_210_096) + Duration::from_millis(7);
        assert_eq!(format_rfc3339_millis(t), "2024-02-29T12:34:56.007Z");
        assert_eq!(
            format_rfc3339_millis(UNIX_EPOCH),
            "1970-01-01T00:00:00.000Z"
        );
    }

    #[test]
    fn a_time_before_the_epoch_is_the_epoch() {
        assert_eq!(
            format_rfc3339(UNIX_EPOCH - Duration::from_secs(5)),
            "1970-01-01T00:00:00Z"
        );
    }

    #[test]
    fn now_is_well_formed() {
        let now = now_rfc3339();
        assert_eq!(now.len(), "2024-02-29T12:34:56Z".len(), "{now}");
        assert!(now.ends_with('Z') && now.as_bytes()[10] == b'T', "{now}");
        assert!(now_rfc3339_millis().len() == now.len() + 4);
        assert!(now_secs() > 1_700_000_000);
    }
}
