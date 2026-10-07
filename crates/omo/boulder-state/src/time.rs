//! `Date.toISOString()`-shaped UTC formatting and ISO-8601 parsing.
//!
//! `chrono`/`time` are not dependencies of this crate, and the TypeScript package only
//! needs the two operations below: formatting the current instant as
//! `YYYY-MM-DDTHH:MM:SS.mmmZ`, and `Date.parse` for ISO-8601 input.

use std::time::{SystemTime, UNIX_EPOCH};

const MILLIS_PER_DAY: i64 = 86_400_000;
const MILLIS_PER_HOUR: i64 = 3_600_000;
const MILLIS_PER_MINUTE: i64 = 60_000;

/// Current Unix time in milliseconds (`Date.now()`).
pub(crate) fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| i64::try_from(elapsed.as_millis()).ok())
        .unwrap_or_default()
}

/// Current time as `YYYY-MM-DDTHH:MM:SS.mmmZ`, matching `new Date().toISOString()`.
pub(crate) fn now_iso_string() -> String {
    format_iso_millis(now_millis())
}

/// Format Unix milliseconds as `YYYY-MM-DDTHH:MM:SS.mmmZ`.
pub(crate) fn format_iso_millis(millis: i64) -> String {
    let days = millis.div_euclid(MILLIS_PER_DAY);
    let millis_of_day = millis.rem_euclid(MILLIS_PER_DAY);
    let (year, month, day) = civil_from_days(days);
    let hours = millis_of_day / MILLIS_PER_HOUR;
    let minutes = (millis_of_day / MILLIS_PER_MINUTE) % 60;
    let seconds = (millis_of_day / 1_000) % 60;
    let millis_part = millis_of_day % 1_000;
    format!("{year:04}-{month:02}-{day:02}T{hours:02}:{minutes:02}:{seconds:02}.{millis_part:03}Z")
}

/// Parse an ISO-8601 timestamp into Unix milliseconds.
///
/// Accepts `YYYY-MM-DD`, `YYYY-MM-DDTHH:MM`, `YYYY-MM-DDTHH:MM:SS[.fff]` and the `Z`,
/// `+HH:MM`, `-HH:MM`, `+HHMM` and `+HH` zone forms. Anything else — including the
/// non-date strings callers store to opt out of timing — yields `None` rather than a
/// panic.
pub(crate) fn parse_iso_to_millis(value: &str) -> Option<i64> {
    let text = value.trim();
    let (date_text, time_text) = match text.split_once(['T', 't']) {
        Some((date, time)) => (date, time),
        None => (text, ""),
    };

    let (year, month, day) = parse_date(date_text)?;
    if month == 0 || month > 12 || day == 0 || day > days_in_month(year, month) {
        return None;
    }

    let (hours, minutes, seconds, millis, offset_minutes) = parse_time(time_text)?;
    let days = days_from_civil(year, month, day);
    let millis_of_day = i64::from(hours) * MILLIS_PER_HOUR
        + i64::from(minutes) * MILLIS_PER_MINUTE
        + i64::from(seconds) * 1_000
        + i64::from(millis)
        - i64::from(offset_minutes) * MILLIS_PER_MINUTE;
    Some(days * MILLIS_PER_DAY + millis_of_day)
}

fn parse_date(text: &str) -> Option<(i64, u32, u32)> {
    let mut parts = text.split('-');
    let year = parts.next()?;
    let month = parts.next()?;
    let day = parts.next()?;
    if parts.next().is_some() {
        return None;
    }
    let year = parse_digits(year, 4)?;
    let month = parse_digits(month, 2)?;
    let day = parse_digits(day, 2)?;
    Some((i64::from(year), month, day))
}

fn parse_time(text: &str) -> Option<(u32, u32, u32, u32, i32)> {
    if text.is_empty() {
        return Some((0, 0, 0, 0, 0));
    }

    let (clock, offset_minutes) = split_zone(text)?;
    let mut parts = clock.split(':');
    let hours = parse_digits(parts.next()?, 2)?;
    let minutes = parse_digits(parts.next()?, 2)?;
    let seconds_field = parts.next().unwrap_or("00");
    if parts.next().is_some() {
        return None;
    }
    let (seconds, millis) = match seconds_field.split_once('.') {
        Some((seconds, fraction)) => (parse_digits(seconds, 2)?, parse_fraction(fraction)?),
        None => (parse_digits(seconds_field, 2)?, 0),
    };
    if hours > 23 || minutes > 59 || seconds > 59 {
        return None;
    }
    Some((hours, minutes, seconds, millis, offset_minutes))
}

fn split_zone(text: &str) -> Option<(&str, i32)> {
    if let Some(clock) = text.strip_suffix(['Z', 'z']) {
        return Some((clock, 0));
    }
    match text.rfind(['+', '-']) {
        Some(index) if index > 0 => {
            let (clock, zone) = text.split_at(index);
            Some((clock, parse_zone_offset(zone)?))
        }
        _ => Some((text, 0)),
    }
}

fn parse_zone_offset(zone: &str) -> Option<i32> {
    let (sign, rest) = match zone.as_bytes().first().copied()? {
        b'+' => (1, zone.get(1..)?),
        b'-' => (-1, zone.get(1..)?),
        _ => return None,
    };
    let (hours, minutes) = match rest.split_once(':') {
        Some((hours, minutes)) => (parse_digits(hours, 2)?, parse_digits(minutes, 2)?),
        None if rest.len() == 4 => (
            parse_digits(rest.get(..2)?, 2)?,
            parse_digits(rest.get(2..)?, 2)?,
        ),
        None if rest.len() == 2 => (parse_digits(rest, 2)?, 0),
        None => return None,
    };
    if hours > 23 || minutes > 59 {
        return None;
    }
    Some(sign * (i32::try_from(hours).ok()? * 60 + i32::try_from(minutes).ok()?))
}

fn parse_fraction(text: &str) -> Option<u32> {
    if text.is_empty() || text.len() > 9 || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let mut padded = String::from(text);
    while padded.len() < 3 {
        padded.push('0');
    }
    padded.truncate(3);
    padded.parse().ok()
}

fn parse_digits(text: &str, expected_len: usize) -> Option<u32> {
    if text.len() != expected_len || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => 0,
    }
}

fn is_leap_year(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant's algorithm).
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let adjusted_year = if month <= 2 { year - 1 } else { year };
    let era = if adjusted_year >= 0 {
        adjusted_year
    } else {
        adjusted_year - 399
    } / 400;
    let year_of_era = adjusted_year - era * 400;
    let shifted_month = (if month > 2 { month - 3 } else { month + 9 }) as i64;
    let day_of_year = (153 * shifted_month + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// Civil date for a day count since 1970-01-01 (Howard Hinnant's algorithm).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let shifted = days + 719_468;
    let era = if shifted >= 0 {
        shifted
    } else {
        shifted - 146_096
    } / 146_097;
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * shifted_month + 2) / 5 + 1) as u32;
    let month = (if shifted_month < 10 {
        shifted_month + 3
    } else {
        shifted_month - 9
    }) as u32;
    let year = if month <= 2 { year + 1 } else { year };
    (year, month, day)
}

#[cfg(test)]
#[path = "time_tests.rs"]
mod tests;
