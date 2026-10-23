//! Wall-clock reading and timeout arithmetic.
//!
//! The plugin reads the current time from the system clock, which the WASI
//! runtime provides to every plugin instance, and it reads history timestamps
//! from the ISO dates in Lidarr's records. Both are UTC, which is all the
//! plugin needs: how long a download may wait to appear in the queue.
//!
//! The clock is a trait so the download logic can be tested with a fixed
//! instant. Everything here is pure: it never touches the network, so it is
//! unit-tested on the host target.

use std::time::{SystemTime, UNIX_EPOCH};

/// A source of the current instant.
pub(crate) trait Clock {
    /// Returns the current instant in seconds since the Unix epoch.
    ///
    /// # Returns
    ///
    /// The instant, and `None` when the system clock is unavailable or reads a
    /// time before the epoch.
    fn now(&self) -> Option<i64>;
}

/// The system clock.
pub(crate) struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Option<i64> {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|elapsed| i64::try_from(elapsed.as_secs()).ok())
    }
}

/// Parses an ISO 8601 timestamp in UTC, as Lidarr writes history dates.
///
/// Lidarr serializes dates like `2024-05-01T12:34:56.789Z`; the fractional
/// second and its digits are accepted and ignored, and a trailing `Z` or no
/// suffix at all means UTC. An offset is not accepted, because a history record
/// the plugin cannot place in time must not decide a download's fate.
///
/// # Parameters
///
/// - `value`: the timestamp.
///
/// # Returns
///
/// The instant, in seconds since the Unix epoch.
///
/// # Example
///
/// ```text
/// parse_iso_utc("1970-01-01T00:01:00Z") -> Some(60)
/// ```
pub fn parse_iso_utc(value: &str) -> Option<i64> {
    let value = value.trim();
    let (date, rest) = value.split_once(['T', ' '])?;
    let mut date_parts = date.split('-');
    let year: i64 = date_parts.next()?.parse().ok()?;
    let month: i64 = date_parts.next()?.parse().ok()?;
    let day: i64 = date_parts.next()?.parse().ok()?;
    if date_parts.next().is_some() {
        return None;
    }

    let (time, suffix) = match rest.split_once('.') {
        Some((time, fraction)) => {
            let end = fraction
                .find(|character: char| !character.is_ascii_digit())
                .unwrap_or(fraction.len());
            (time, &fraction[end..])
        }
        None => match rest.strip_suffix('Z') {
            Some(time) => (time, "Z"),
            None => (rest, ""),
        },
    };
    if !suffix.is_empty() && suffix != "Z" {
        return None;
    }

    let (hour, minute, second) = parse_time(time)?;

    seconds(year, month, day, hour, minute, second)
}

/// Returns whether a download has waited longer than its configured limit.
///
/// # Parameters
///
/// - `started_at`: when the album search was issued, in seconds since the Unix
///   epoch.
/// - `now`: the current instant, in the same unit.
/// - `wait_minutes`: how long Lidarr may take to move the album into its queue.
///
/// # Returns
///
/// `true` once the wait has passed. When either instant is unknown the download
/// is never timed out, because a missing clock reading must not fail a download
/// that may well be progressing.
///
/// # Example
///
/// ```text
/// timed_out(Some(0), Some(1801), 30) -> true
/// timed_out(Some(0), Some(1799), 30) -> false
/// ```
pub fn timed_out(started_at: Option<i64>, now: Option<i64>, wait_minutes: u32) -> bool {
    match (started_at, now) {
        (Some(started_at), Some(now)) => {
            now.saturating_sub(started_at) > i64::from(wait_minutes) * 60
        }
        _ => false,
    }
}

/// Parses an `hour:minute:second` time.
///
/// # Parameters
///
/// - `value`: the time field.
///
/// # Returns
///
/// The three components. ISO timestamps carry their fraction separately, so a
/// fraction in the time field is rejected.
fn parse_time(value: &str) -> Option<(i64, i64, i64)> {
    let mut parts = value.split(':');
    let hour: i64 = parts.next()?.parse().ok()?;
    let minute: i64 = parts.next()?.parse().ok()?;
    let second: i64 = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }

    Some((hour, minute, second))
}

/// Converts a UTC calendar date and time to seconds since the Unix epoch.
///
/// Field ranges are checked, so a malformed date is rejected rather than
/// silently wrapped. The day count uses Howard Hinnant's `days_from_civil`,
/// which is exact for every proleptic Gregorian date.
///
/// # Parameters
///
/// - `year`: the calendar year.
/// - `month`: the month, one-based.
/// - `day`: the day of the month.
/// - `hour`: the hour of the day.
/// - `minute`: the minute of the hour.
/// - `second`: the second of the minute.
///
/// # Returns
///
/// The instant, in seconds since the Unix epoch.
fn seconds(year: i64, month: i64, day: i64, hour: i64, minute: i64, second: i64) -> Option<i64> {
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || !(0..24).contains(&hour)
        || !(0..60).contains(&minute)
        || !(0..=60).contains(&second)
    {
        return None;
    }

    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let shifted_month = (month + 9) % 12;
    let day_of_year = (153 * shifted_month + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;

    Some(days * 86_400 + hour * 3_600 + minute * 60 + second)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_system_clock() {
        let now = SystemClock.now().unwrap();

        assert!(now > 1_600_000_000, "{now}");
    }

    #[test]
    fn parses_an_iso_timestamp() {
        assert_eq!(parse_iso_utc("1970-01-01T00:01:00Z"), Some(60));
        assert_eq!(
            parse_iso_utc("2024-05-01T12:34:56.789Z"),
            Some(1_714_566_896)
        );
        assert_eq!(parse_iso_utc("2024-05-01T12:34:56"), Some(1_714_566_896));
    }

    #[test]
    fn rejects_an_iso_timestamp_with_an_offset() {
        assert_eq!(parse_iso_utc("2024-05-01T12:34:56+02:00"), None);
        assert_eq!(parse_iso_utc("not a date"), None);
    }

    #[test]
    fn times_out_only_after_the_wait() {
        assert!(!timed_out(Some(0), Some(1799), 30));
        assert!(timed_out(Some(0), Some(1801), 30));
        assert!(!timed_out(None, Some(1801), 30));
        assert!(!timed_out(Some(0), None, 30));
    }
}
