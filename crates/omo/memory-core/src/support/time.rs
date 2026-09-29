//! Unix-millisecond timestamps, ISO-8601 (RFC 3339) formatting and parsing.
//!
//! The TypeScript package uses `Date.toISOString()` and `Date.parse`; the port
//! reproduces both without pulling in a date crate.

use std::time::{SystemTime, UNIX_EPOCH};

/// Current wall-clock time in milliseconds since the Unix epoch.
pub fn now_millis() -> i64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(elapsed) => elapsed.as_millis() as i64,
        Err(_) => 0,
    }
}

/// Current wall-clock time formatted exactly like `new Date().toISOString()`.
pub fn now_iso() -> String {
    format_rfc3339_millis(now_millis())
}

/// `2026-09-28T12:34:56.789Z` from milliseconds since the epoch.
pub fn format_rfc3339_millis(millis: i64) -> String {
    let seconds = millis.div_euclid(1000);
    let sub_millis = millis.rem_euclid(1000);
    let days = seconds.div_euclid(86_400);
    let second_of_day = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let hour = second_of_day / 3_600;
    let minute = (second_of_day % 3_600) / 60;
    let second = second_of_day % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{sub_millis:03}Z")
}

/// Parse an ISO-8601 / RFC 3339 timestamp into milliseconds since the epoch.
///
/// Accepts `YYYY-MM-DD`, `YYYY-MM-DDTHH:MM:SS`, optional fractional seconds,
/// and either `Z` or a `+HH:MM` / `-HH:MM` offset. Returns `None` for anything
/// else, matching `Number.isNaN(Date.parse(...))` for the shapes memory-core
/// actually produces and consumes.
pub fn parse_rfc3339(input: &str) -> Option<i64> {
    let bytes = input.as_bytes();
    if bytes.len() < 10 {
        return None;
    }
    let year: i64 = parse_digits(input.get(0..4)?)?;
    if bytes[4] != b'-' {
        return None;
    }
    let month: i64 = parse_digits(input.get(5..7)?)?;
    if bytes[7] != b'-' {
        return None;
    }
    let day: i64 = parse_digits(input.get(8..10)?)?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }

    let mut millis = days_from_civil(year, month as u32, day as u32) * 86_400_000;
    if bytes.len() == 10 {
        return Some(millis);
    }
    if bytes[10] != b'T' && bytes[10] != b't' && bytes[10] != b' ' {
        return None;
    }
    let hour: i64 = parse_digits(input.get(11..13)?)?;
    if bytes.get(13) != Some(&b':') {
        return None;
    }
    let minute: i64 = parse_digits(input.get(14..16)?)?;
    if hour > 23 || minute > 59 {
        return None;
    }
    millis += hour * 3_600_000 + minute * 60_000;

    let mut cursor = 16;
    if bytes.get(16) == Some(&b':') {
        let second: i64 = parse_digits(input.get(17..19)?)?;
        if second > 60 {
            return None;
        }
        millis += second * 1_000;
        cursor = 19;
    }
    if bytes.get(cursor) == Some(&b'.') {
        let fraction_start = cursor + 1;
        let mut end = fraction_start;
        while bytes.get(end).is_some_and(u8::is_ascii_digit) {
            end += 1;
        }
        let digits = input.get(fraction_start..end)?;
        let mut scale = 100i64;
        let mut value = 0i64;
        for digit in digits.bytes() {
            if scale == 0 {
                break;
            }
            value += i64::from(digit - b'0') * scale;
            scale /= 10;
        }
        millis += value;
        cursor = end;
    }

    match bytes.get(cursor) {
        Some(b'Z') | Some(b'z') => {
            if cursor + 1 == bytes.len() {
                Some(millis)
            } else {
                None
            }
        }
        Some(sign @ (b'+' | b'-')) => {
            let offset_hours: i64 = parse_digits(input.get(cursor + 1..cursor + 3)?)?;
            let offset_minutes: i64 = if bytes.get(cursor + 3) == Some(&b':') {
                parse_digits(input.get(cursor + 4..cursor + 6)?)?
            } else {
                parse_digits(input.get(cursor + 3..cursor + 5)?)?
            };
            let offset = offset_hours * 3_600_000 + offset_minutes * 60_000;
            Some(if *sign == b'+' {
                millis - offset
            } else {
                millis + offset
            })
        }
        None => Some(millis),
        Some(_) => None,
    }
}

fn parse_digits(input: &str) -> Option<i64> {
    if input.is_empty() || !input.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    input.parse::<i64>().ok()
}

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant's algorithm).
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month = i64::from(month);
    let day_of_year =
        (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// Inverse of [`days_from_civil`].
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let shifted = days + 719_468;
    let era = if shifted >= 0 {
        shifted
    } else {
        shifted - 146_096
    } / 146_097;
    let day_of_era = (shifted - era * 146_097) as u64;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era as i64 + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let mp = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

#[cfg(test)]
#[path = "time_tests.rs"]
mod tests;
