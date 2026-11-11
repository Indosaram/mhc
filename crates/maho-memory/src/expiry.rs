//! The `expires` frontmatter key: the instant after which a memory file stops being projected.
//!
//! Spec: `docs/cleanroom/specs/memory-expiry.md`. Only instants that mean the same thing on every device are
//! accepted; anything else reads as "no expiry", because a malformed value must never hide content.

use crate::frontmatter::decode_scalar_source;
use crate::normalize::MemoryFrontmatter;

pub fn memory_expires_at_ms(frontmatter: &MemoryFrontmatter) -> Option<i64> {
    let raw = frontmatter.extra.as_ref()?.get("expires")?;
    parse_expiry_instant(decode_scalar_source(raw.trim()).trim())
}

/// Expiry is inclusive of its own instant: at `expires_at_ms` the file is already gone.
pub fn is_memory_expired(expires_at_ms: Option<i64>, as_of_ms: i64) -> bool {
    expires_at_ms.is_some_and(|at| as_of_ms >= at)
}

/// A whole `YYYY-MM-DD` date, a datetime with `Z`, or a datetime with a numeric offset.
///
/// An offset-less datetime is refused deliberately: `2026-10-27T06:33` is a different instant in every
/// timezone, so a memory store shared across machines would expire the file at different moments per device.
pub fn parse_expiry_instant(value: &str) -> Option<i64> {
    let bytes = value.as_bytes();
    if bytes.len() < 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return None;
    }
    let year = digits(bytes, 0, 4)?;
    let month = digits(bytes, 5, 2)?;
    let day = digits(bytes, 8, 2)?;
    let day_ms = days_from_civil(year, month, day)? * 86_400_000;
    if bytes.len() == 10 {
        return Some(day_ms);
    }
    if bytes[10] != b'T' || bytes.get(13) != Some(&b':') {
        return None;
    }
    let hour = digits(bytes, 11, 2).filter(|hour| *hour <= 23)?;
    let minute = digits(bytes, 14, 2).filter(|minute| *minute <= 59)?;
    let mut at = 16;
    let mut second = 0;
    let mut millis = 0;
    if bytes.get(at) == Some(&b':') {
        second = digits(bytes, at + 1, 2).filter(|second| *second <= 59)?;
        at += 3;
        if bytes.get(at) == Some(&b'.') {
            let fraction = bytes[at + 1..]
                .iter()
                .take_while(|byte| byte.is_ascii_digit())
                .count();
            if !(1..=9).contains(&fraction) {
                return None;
            }
            // Millisecond precision: further digits truncate, the same way `Date.parse` reads them.
            millis = (0..3).fold(0, |sum, index| {
                sum * 10
                    + bytes
                        .get(at + 1 + index)
                        .filter(|_| index < fraction)
                        .map_or(0, |byte| i64::from(byte - b'0'))
            });
            at += 1 + fraction;
        }
    }
    let offset_ms = match &bytes[at..] {
        [b'Z'] => 0,
        [sign @ (b'+' | b'-'), rest @ ..] if rest.len() == 5 && rest[2] == b':' => {
            let offset_hour = digits(rest, 0, 2).filter(|hour| *hour <= 23)?;
            let offset_minute = digits(rest, 3, 2).filter(|minute| *minute <= 59)?;
            let magnitude = (offset_hour * 60 + offset_minute) * 60_000;
            if *sign == b'+' { magnitude } else { -magnitude }
        }
        _ => return None,
    };
    Some(day_ms + ((hour * 60 + minute) * 60 + second) * 1000 + millis - offset_ms)
}

fn digits(bytes: &[u8], start: usize, count: usize) -> Option<i64> {
    let slice = bytes.get(start..start + count)?;
    slice.iter().all(u8::is_ascii_digit).then(|| {
        slice
            .iter()
            .fold(0, |sum, byte| sum * 10 + i64::from(byte - b'0'))
    })
}

/// Days since 1970-01-01 for a proleptic Gregorian date, or `None` for a day that does not exist.
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
    let shifted_year = if month <= 2 { year - 1 } else { year };
    let era = shifted_year.div_euclid(400);
    let year_of_era = shifted_year - era * 400;
    let day_of_year = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    Some(era * 146_097 + day_of_era - 719_468)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY_MS: i64 = 86_400_000;

    fn frontmatter(expires: &str) -> MemoryFrontmatter {
        let mut extra = std::collections::BTreeMap::new();
        extra.insert("expires".to_owned(), expires.to_owned());
        MemoryFrontmatter {
            description: "d".to_owned(),
            read_only: None,
            kind: None,
            aliases: None,
            extra: Some(extra),
        }
    }

    fn ms(at: &str) -> i64 {
        parse_expiry_instant(at).expect("instant")
    }

    #[test]
    fn a_date_expires_at_the_start_of_that_utc_day() {
        assert_eq!(ms("2026-10-28"), 1_793_145_600_000);
        assert_eq!(ms("1970-01-01"), 0);
    }

    #[test]
    fn a_zulu_datetime_parses() {
        assert_eq!(ms("1970-01-01T00:00:00Z"), 0);
        assert_eq!(ms("2026-10-27T06:33Z"), 1_793_082_780_000);
    }

    #[test]
    fn a_numeric_offset_shifts_the_instant() {
        assert_eq!(ms("2026-10-27T15:33:00+09:00"), ms("2026-10-27T06:33:00Z"));
        assert_eq!(ms("2026-10-27T01:33:00-05:00"), ms("2026-10-27T06:33:00Z"));
    }

    #[test]
    fn a_datetime_without_an_offset_is_refused() {
        assert_eq!(parse_expiry_instant("2026-10-27T06:33"), None);
        assert_eq!(parse_expiry_instant("2026-10-27T06:33:00"), None);
    }

    #[test]
    fn seconds_and_milliseconds_are_optional_and_additive() {
        let base = ms("2026-10-27T06:33Z");
        assert_eq!(ms("2026-10-27T06:33:00Z"), base);
        assert_eq!(ms("2026-10-27T06:33:07Z"), base + 7_000);
        assert_eq!(ms("2026-10-27T06:33:07.250Z"), base + 7_250);
    }

    #[test]
    fn sub_millisecond_digits_truncate() {
        assert_eq!(
            ms("2026-10-27T06:33:07.250999Z"),
            ms("2026-10-27T06:33:07.250Z")
        );
    }

    #[test]
    fn a_fraction_without_digits_is_refused() {
        assert_eq!(parse_expiry_instant("2026-10-27T06:33:07.Z"), None);
    }

    #[test]
    fn an_impossible_calendar_date_is_refused() {
        for value in ["2026-02-30", "2026-13-01", "2026-00-01", "2026-11-31"] {
            assert_eq!(parse_expiry_instant(value), None, "{value}");
        }
    }

    #[test]
    fn leap_years_follow_the_calendar() {
        assert!(parse_expiry_instant("2024-02-29").is_some());
        assert_eq!(parse_expiry_instant("2026-02-29"), None);
        assert!(parse_expiry_instant("2000-02-29").is_some());
        assert_eq!(parse_expiry_instant("1900-02-29"), None);
    }

    #[test]
    fn out_of_range_time_fields_are_refused() {
        for value in [
            "2026-10-27T24:00Z",
            "2026-10-27T06:60Z",
            "2026-10-27T06:33:60Z",
            "2026-10-27T06:33+24:00",
            "2026-10-27T06:33+00:60",
        ] {
            assert_eq!(parse_expiry_instant(value), None, "{value}");
        }
    }

    #[test]
    fn free_text_and_mis_shaped_values_are_refused() {
        for value in [
            "",
            "tomorrow",
            "2026-1-01",
            "26-10-27",
            "2026/10/27",
            "2026-10-27 06:33Z",
        ] {
            assert_eq!(parse_expiry_instant(value), None, "{value}");
        }
    }

    #[test]
    fn a_negative_offset_moves_the_instant_later() {
        assert!(ms("2026-10-27T00:00:00-05:00") > ms("2026-10-27T00:00:00Z"));
    }

    #[test]
    fn days_are_exactly_one_day_apart() {
        assert_eq!(ms("2026-10-28") - ms("2026-10-27"), DAY_MS);
    }

    #[test]
    fn a_frontmatter_without_an_extra_block_has_no_expiry() {
        let frontmatter = MemoryFrontmatter {
            description: "d".to_owned(),
            read_only: None,
            kind: None,
            aliases: None,
            extra: None,
        };
        assert_eq!(memory_expires_at_ms(&frontmatter), None);
    }

    #[test]
    fn a_frontmatter_without_an_expires_key_has_no_expiry() {
        let mut extra = std::collections::BTreeMap::new();
        extra.insert("other".to_owned(), "2026-10-28".to_owned());
        let frontmatter = MemoryFrontmatter {
            description: "d".to_owned(),
            read_only: None,
            kind: None,
            aliases: None,
            extra: Some(extra),
        };
        assert_eq!(memory_expires_at_ms(&frontmatter), None);
    }

    #[test]
    fn the_expires_value_is_read_from_the_frontmatter() {
        assert_eq!(
            memory_expires_at_ms(&frontmatter("2026-10-28")),
            Some(1_793_145_600_000)
        );
    }

    #[test]
    fn a_quoted_expires_value_reads_the_same_as_an_unquoted_one() {
        assert_eq!(
            memory_expires_at_ms(&frontmatter("\"2026-10-28\"")),
            memory_expires_at_ms(&frontmatter("2026-10-28"))
        );
    }

    #[test]
    fn surrounding_whitespace_is_tolerated() {
        assert_eq!(
            memory_expires_at_ms(&frontmatter("  2026-10-28  ")),
            memory_expires_at_ms(&frontmatter("2026-10-28"))
        );
    }

    #[test]
    fn a_malformed_expires_value_means_no_expiry() {
        assert_eq!(memory_expires_at_ms(&frontmatter("soon")), None);
    }

    #[test]
    fn a_file_is_live_until_its_instant() {
        let at = ms("2026-10-28");
        assert!(!is_memory_expired(Some(at), at - 1));
        assert!(is_memory_expired(Some(at), at));
        assert!(is_memory_expired(Some(at), at + 1));
    }

    #[test]
    fn no_declared_expiry_is_never_expired() {
        assert!(!is_memory_expired(None, i64::MAX));
    }
}
