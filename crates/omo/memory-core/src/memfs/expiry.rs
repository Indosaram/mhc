//! The `expires` frontmatter key: the instant after which a memory file is no longer projected.
//!
//! Accepted values are ISO 8601 with an unambiguous instant:
//! - a date (`2026-10-28`): the start of that UTC day;
//! - a datetime with `Z` or a numeric offset (`2026-10-27T06:33Z`, `2026-10-27T15:33:00+09:00`).
//!
//! Anything else (an offset-less datetime, which means a different instant on each device; free
//! text; an impossible calendar date) reads as "no expiry", so a malformed value never hides content.

use super::frontmatter::MemoryFrontmatter;
use super::frontmatter_scalar::decode_scalar_source;

/// The instant (epoch ms) the file stops being projected, or `None` when it declares no readable expiry.
pub fn memory_expires_at_ms(frontmatter: &MemoryFrontmatter) -> Option<i64> {
    let raw = frontmatter.extra.as_ref()?.get("expires")?;
    parse_expiry_instant(&decode_scalar_source(raw.trim()))
}

/// A file is expired from its instant on: at `expires_at_ms` itself it is no longer projected.
pub fn is_memory_expired(expires_at_ms: Option<i64>, as_of_ms: i64) -> bool {
    expires_at_ms.is_some_and(|at| as_of_ms >= at)
}

fn digits(bytes: &[u8], start: usize, count: usize) -> Option<i64> {
    let slice = bytes.get(start..start + count)?;
    slice.iter().all(u8::is_ascii_digit).then(|| {
        slice
            .iter()
            .fold(0, |sum, b| sum * 10 + i64::from(b - b'0'))
    })
}

/// Days since 1970-01-01 of a proleptic Gregorian date; `None` for an impossible calendar date.
fn days_from_civil(year: i64, month: i64, day: i64) -> Option<i64> {
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    let month_days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return None,
    };
    if !(1..=month_days).contains(&day) {
        return None;
    }
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146_097 + doe - 719_468)
}

fn parse_expiry_instant(value: &str) -> Option<i64> {
    let bytes = value.as_bytes();
    if bytes.len() < 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return None;
    }
    let (year, month, day) = (
        digits(bytes, 0, 4)?,
        digits(bytes, 5, 2)?,
        digits(bytes, 8, 2)?,
    );
    let day_ms = days_from_civil(year, month, day)? * 86_400_000;
    if bytes.len() == 10 {
        return Some(day_ms);
    }
    if bytes[10] != b'T' || bytes.get(13) != Some(&b':') {
        return None;
    }
    let hour = digits(bytes, 11, 2).filter(|h| *h <= 23)?;
    let minute = digits(bytes, 14, 2).filter(|m| *m <= 59)?;
    let mut at = 16;
    let (mut second, mut millis) = (0, 0);
    if bytes.get(at) == Some(&b':') {
        second = digits(bytes, at + 1, 2).filter(|s| *s <= 59)?;
        at += 3;
        if bytes.get(at) == Some(&b'.') {
            let fraction = bytes[at + 1..]
                .iter()
                .take_while(|b| b.is_ascii_digit())
                .count();
            if !(1..=9).contains(&fraction) {
                return None;
            }
            // Millisecond precision, like Date.parse: extra digits truncate.
            millis = (0..3).fold(0, |sum, i| {
                sum * 10
                    + bytes
                        .get(at + 1 + i)
                        .filter(|_| i < fraction)
                        .map_or(0, |b| i64::from(b - b'0'))
            });
            at += 1 + fraction;
        }
    }
    let offset_ms = match &bytes[at..] {
        [b'Z'] => 0,
        [sign @ (b'+' | b'-'), rest @ ..] if rest.len() == 5 && rest[2] == b':' => {
            let offset_hour = digits(rest, 0, 2).filter(|h| *h <= 23)?;
            let offset_minute = digits(rest, 3, 2).filter(|m| *m <= 59)?;
            let magnitude = (offset_hour * 60 + offset_minute) * 60_000;
            if *sign == b'+' { magnitude } else { -magnitude }
        }
        _ => return None,
    };
    Some(day_ms + ((hour * 60 + minute) * 60 + second) * 1000 + millis - offset_ms)
}

#[cfg(test)]
#[path = "expiry_tests.rs"]
mod tests;
