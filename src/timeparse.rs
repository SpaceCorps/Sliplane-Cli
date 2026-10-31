//! Durations (`30s`, `5m`, `2h`, `1d`) and RFC 3339 timestamps. A CLI that reads two timestamp
//! formats does not need a date-time crate, and one more dependency is one more thing to audit.

use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::{Error, Result};

pub fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// `30s`, `5m`, `2h` or `1d`, as seconds. Zero and anything unparseable is `None`.
pub fn duration_secs(s: &str) -> Option<u64> {
    let s = s.trim();
    let (digits, multiplier) = [('s', 1), ('m', 60), ('h', 3600), ('d', 86400)]
        .into_iter()
        .find_map(|(unit, seconds)| s.strip_suffix(unit).map(|digits| (digits, seconds)))?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse::<u64>().ok()?.checked_mul(multiplier).filter(|v| *v > 0)
}

/// `2026-10-31T12:00:00Z`, `2026-10-31T14:00:00.5+02:00`, as (Unix seconds, nanoseconds).
pub fn rfc3339(s: &str) -> Option<(i64, u32)> {
    let s = s.trim();
    let num = |from: usize, to: usize| -> Option<i64> {
        let part = s.get(from..to)?;
        part.bytes().all(|b| b.is_ascii_digit()).then(|| part.parse().ok()).flatten()
    };
    let at = |i: usize, c: &[u8]| s.as_bytes().get(i).is_some_and(|b| c.contains(b));
    if !(at(4, b"-") && at(7, b"-") && at(10, b"Tt ") && at(13, b":") && at(16, b":")) {
        return None;
    }
    let (year, month, day) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (hour, minute, second) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    if !(1..=12).contains(&month)
        || day < 1
        || day > days_in_month(year, month)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }

    let mut i = 19;
    let mut nanos = 0u32;
    if at(i, b".") {
        let digits: String = s[i + 1..].chars().take_while(char::is_ascii_digit).collect();
        if digits.is_empty() {
            return None;
        }
        nanos = format!("{:0<9}", &digits[..digits.len().min(9)]).parse().ok()?;
        i += 1 + digits.len();
    }

    let offset = match s.get(i..)? {
        "Z" | "z" => 0,
        tail if tail.len() == 6 && (tail.starts_with('+') || tail.starts_with('-')) && tail.as_bytes()[3] == b':' => {
            let (h, m) = (tail[1..3].parse::<i64>().ok()?, tail[4..6].parse::<i64>().ok()?);
            if h > 23 || m > 59 {
                return None;
            }
            let seconds = h * 3600 + m * 60;
            if tail.starts_with('-') { -seconds } else { seconds }
        }
        _ => return None,
    };

    Some((days_from_civil(year, month, day) * 86400 + hour * 3600 + minute * 60 + second - offset, nanos))
}

fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        2 if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant's algorithm).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let year_of_era = y - era * 400;
    let day_of_year = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146097 + day_of_era - 719468
}

/// Unix seconds as an RFC 3339 UTC timestamp, `2026-10-31T12:00:00Z` - what the API's `date-time`
/// fields take.
pub fn format_rfc3339(secs: i64) -> String {
    let (days, rest) = (secs.div_euclid(86400), secs.rem_euclid(86400));
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z", rest / 3600, rest % 3600 / 60, rest % 60)
}

/// The inverse of [`days_from_civil`] (Howard Hinnant's algorithm): (year, month, day).
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719468;
    let era = (if z >= 0 { z } else { z - 146096 }) / 146097;
    let day_of_era = z - era * 146097;
    let year_of_era = (day_of_era - day_of_era / 1460 + day_of_era / 36524 - day_of_era / 146096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 { shifted_month + 3 } else { shifted_month - 9 };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// What `--since` accepts: how long ago (`30m`), or a point in time (RFC 3339). Unix seconds.
pub fn since(value: &str) -> Result<i64> {
    if let Some(secs) = duration_secs(value) {
        return Ok(now() - secs as i64);
    }
    if let Some((secs, _)) = rfc3339(value) {
        return Ok(secs);
    }
    Err(Error::invalid(format!(
        "--since '{value}' is neither a duration (30s, 5m, 2h, 1d) nor an RFC 3339 timestamp (2026-10-31T12:00:00Z)."
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        assert_eq!(duration_secs("30s"), Some(30));
        assert_eq!(duration_secs("5m"), Some(300));
        assert_eq!(duration_secs("2h"), Some(7200));
        assert_eq!(duration_secs("1d"), Some(86400));
        for bad in ["", "m", "0s", "-5m", "5", "5x", "1.5h", "5 m", "+5m", "٣m"] {
            assert_eq!(duration_secs(bad), None, "{bad:?}");
        }
        assert_eq!(duration_secs("99999999999999999999d"), None);
    }

    #[test]
    fn timestamps() {
        assert_eq!(rfc3339("1970-01-01T00:00:00Z"), Some((0, 0)));
        assert_eq!(rfc3339("2000-03-01T00:00:00Z"), Some((951_868_800, 0)));
        assert_eq!(rfc3339("2024-02-29T12:00:00Z"), Some((1_709_208_000, 0)));
        assert_eq!(rfc3339("1969-12-31T23:59:59Z"), Some((-1, 0)));
        // An offset moves the instant, it does not just decorate it.
        assert_eq!(rfc3339("2026-01-01T02:00:00+02:00"), rfc3339("2026-01-01T00:00:00Z"));
        assert_eq!(rfc3339("2025-12-31T19:00:00-05:00"), rfc3339("2026-01-01T00:00:00Z"));
        assert_eq!(rfc3339("2026-01-01T00:00:00.5Z"), Some((rfc3339("2026-01-01T00:00:00Z").unwrap().0, 500_000_000)));
        assert_eq!(rfc3339("2026-01-01T00:00:00.123456789123Z").unwrap().1, 123_456_789);
        assert_eq!(rfc3339("2026-01-01t00:00:00z"), rfc3339("2026-01-01T00:00:00Z"));
    }

    #[test]
    fn rejects_malformed_timestamps() {
        for bad in [
            "",
            "2026-01-01",
            "2026-01-01T00:00:00",
            "2026-13-01T00:00:00Z",
            "2026-02-30T00:00:00Z",
            "2025-02-29T00:00:00Z",
            "2026-01-01T24:00:00Z",
            "2026-01-01T00:60:00Z",
            "2026-01-01T00:00:00.Z",
            "2026-01-01T00:00:00+2:00",
            "2026-01-01T00:00:00+24:00",
            "2026-01-01T00:00:00Zjunk",
            "2026-01-0１T00:00:00Z",
        ] {
            assert_eq!(rfc3339(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn formats_timestamps() {
        assert_eq!(format_rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_rfc3339(951_868_800), "2000-03-01T00:00:00Z");
        assert_eq!(format_rfc3339(1_709_208_000), "2024-02-29T12:00:00Z");
        assert_eq!(format_rfc3339(-1), "1969-12-31T23:59:59Z");
        assert_eq!(format_rfc3339(1_798_761_599), "2026-12-31T23:59:59Z");
        assert_eq!(format_rfc3339(4_102_444_800), "2100-01-01T00:00:00Z");
    }

    #[test]
    fn formatting_round_trips_through_parsing() {
        // About every six hours from 1960 to 2110: leap days, century years and both era signs.
        let mut secs = -310_000_000i64;
        while secs < 4_400_000_000 {
            let text = format_rfc3339(secs);
            assert_eq!(rfc3339(&text), Some((secs, 0)), "{secs} -> {text}");
            secs += 21_659;
        }
    }

    #[test]
    fn since_takes_either_form() {
        let ago = since("5m").unwrap();
        assert!((now() - 300 - ago).abs() <= 1);
        assert_eq!(since("2000-03-01T00:00:00Z").unwrap(), 951_868_800);
        assert!(since("yesterday").unwrap_err().message.contains("neither a duration"));
    }
}
