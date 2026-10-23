//! Port of senpi packages/coding-agent/src/core/credential-pool/reset-time.ts.
//!
//! When a spent account's usage limit resets, read from the failure text: a JSON reset field or reset
//! prose. Returns milliseconds from `now_ms` (0 for a reset already past) or `None` when no well-formed
//! reset time is present, so the caller keeps its default cooldown.
//!
//! Deviation: an impossible calendar day ("resets Feb 31, 9am") is rejected instead of rolled over as
//! `Date.UTC` does.

use std::sync::LazyLock;

use regex::Regex;
use serde_json::Value;

const MINUTE_MS: f64 = 60_000.0;
const HOUR_MS: f64 = 60.0 * MINUTE_MS;
const DAY_MS: f64 = 24.0 * HOUR_MS;
const MONTHS: [&str; 12] = [
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];

const UNITS: &str = r"days?|d|hours?|hrs?|h|minutes?|mins?|m|seconds?|secs?|s";

/// "resets in 3 hours", "try again in 2h 30m", "retry in 45 minutes".
static RELATIVE: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(&format!(r"(?i)\b(?:resets?|try again|retry)\s+in\s+~?((?:\d+(?:\.\d+)?\s*(?:{UNITS})\b(?:\s*,?\s*(?:and\s+)?)?)+)")).ok()
});
static RELATIVE_PART: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(&format!(r"(?i)(\d+(?:\.\d+)?)\s*({UNITS})\b")).ok());
/// "resets 12am (Asia/Seoul)", "resets Oct 2, 9am", "try again at 12:00 AM".
static CLOCK: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\b(?:resets?|try again)\s+(?:at\s+|on\s+)?(?:([a-z]{3})[a-z]*\.?\s+(\d{1,2}),?\s+(?:at\s+)?)?(\d{1,2})(?::(\d{2}))?\s*([ap])\.?\s?m\b\.?(?:\s*\(([A-Za-z_]+(?:/[A-Za-z0-9_+-]+)*)\))?",
    )
    .ok()
});
static ISO_DAY: LazyLock<Option<Regex>> = LazyLock::new(|| Regex::new(r"\d{4}-\d{2}-\d{2}T").ok());

/// Whether the reset names a moment or a duration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResetKind {
    Absolute,
    Relative,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UsageLimitReset {
    pub ms: u64,
    pub kind: ResetKind,
}

/// `usageLimitResetMs`.
pub fn usage_limit_reset_ms(text: &str, now_ms: i64) -> Option<u64> {
    usage_limit_reset(text, now_ms).map(|reset| reset.ms)
}

/// `usageLimitReset`: the reset with its kind (`Absolute`: resets_at, "resets 3pm"; `Relative`: a duration).
pub fn usage_limit_reset(text: &str, now_ms: i64) -> Option<UsageLimitReset> {
    if let Some(ms) = from_json_fields(text, now_ms) {
        return Some(UsageLimitReset {
            ms,
            kind: ResetKind::Absolute,
        });
    }
    if let Some(ms) = from_relative_prose(text) {
        return Some(UsageLimitReset {
            ms,
            kind: ResetKind::Relative,
        });
    }
    from_clock_prose(text, now_ms).map(|ms| UsageLimitReset {
        ms,
        kind: ResetKind::Absolute,
    })
}

fn clamp_ms(value: f64) -> u64 {
    // Non-finite or negative reads as "already past"; the cast saturates for absurd values.
    if value.is_finite() && value > 0.0 {
        value as u64
    } else {
        0
    }
}

fn from_json_fields(text: &str, now_ms: i64) -> Option<u64> {
    let start = text.find('{')?;
    let parsed: Value = serde_json::from_str(&text[start..]).ok()?;
    let mut found: Option<u64> = None;
    walk(&parsed, &mut |key, value| {
        let ms = match key {
            "resets_at" => absolute_field_ms(value, now_ms),
            "reset_after_seconds" => seconds_field_ms(value),
            _ => None,
        };
        if let Some(ms) = ms
            && found.is_none_or(|current| ms > current)
        {
            found = Some(ms);
        }
    });
    found
}

fn walk(value: &Value, visit: &mut impl FnMut(&str, &Value)) {
    match value {
        Value::Array(items) => items.iter().for_each(|item| walk(item, visit)),
        Value::Object(map) => {
            for (key, entry) in map {
                visit(key, entry);
                walk(entry, visit);
            }
        }
        _ => {}
    }
}

fn absolute_field_ms(value: &Value, now_ms: i64) -> Option<u64> {
    match value {
        Value::Number(number) => {
            let raw = number
                .as_f64()
                .filter(|raw| raw.is_finite() && *raw > 0.0)?;
            let epoch_ms = if raw < 1e12 { raw * 1000.0 } else { raw };
            Some(clamp_ms((epoch_ms - now_ms as f64).ceil()))
        }
        Value::String(text)
            if ISO_DAY
                .as_ref()
                .is_some_and(|pattern| pattern.is_match(text)) =>
        {
            let at = text.parse::<jiff::Timestamp>().ok()?;
            Some(clamp_ms((at.as_millisecond() - now_ms) as f64))
        }
        _ => None,
    }
}

fn seconds_field_ms(value: &Value) -> Option<u64> {
    let seconds = value
        .as_f64()
        .filter(|seconds| seconds.is_finite() && *seconds >= 0.0)?;
    Some(clamp_ms((seconds * 1000.0).ceil()))
}

fn unit_ms(unit: &str) -> f64 {
    let unit = unit.to_lowercase();
    if unit.starts_with('d') {
        DAY_MS
    } else if unit.starts_with('h') {
        HOUR_MS
    } else if unit == "s" || unit.starts_with("sec") {
        1000.0
    } else {
        MINUTE_MS
    }
}

fn from_relative_prose(text: &str) -> Option<u64> {
    let phrase = RELATIVE.as_ref()?.captures(text)?.get(1)?.as_str();
    let mut total = 0.0;
    for part in RELATIVE_PART.as_ref()?.captures_iter(phrase) {
        let amount = part
            .get(1)
            .and_then(|amount| amount.as_str().parse::<f64>().ok())
            .unwrap_or(0.0);
        total += amount * unit_ms(part.get(2).map_or("", |unit| unit.as_str()));
    }
    Some(clamp_ms(total.ceil()))
}

fn zoned_at(
    zone: &jiff::tz::TimeZone,
    date: jiff::civil::Date,
    hour: i8,
    minute: i8,
) -> Option<i64> {
    let wall = date.at(hour, minute, 0, 0);
    zone.to_zoned(wall)
        .ok()
        .map(|zoned| zoned.timestamp().as_millisecond())
}

fn from_clock_prose(text: &str, now_ms: i64) -> Option<u64> {
    let captures = CLOCK.as_ref()?.captures(text)?;
    let hour12: i8 = captures.get(3)?.as_str().parse().ok()?;
    let minute: i8 = captures
        .get(4)
        .map_or(Some(0), |minute| minute.as_str().parse().ok())?;
    if !(1..=12).contains(&hour12) || minute > 59 {
        return None;
    }
    let pm = captures
        .get(5)
        .is_some_and(|meridiem| meridiem.as_str().eq_ignore_ascii_case("p"));
    let hour = hour12 % 12 + if pm { 12 } else { 0 };
    let zone = match captures.get(6) {
        Some(name) => jiff::tz::TimeZone::get(name.as_str()).ok()?,
        None => jiff::tz::TimeZone::system(),
    };
    let today = jiff::Timestamp::from_millisecond(now_ms)
        .ok()?
        .to_zoned(zone.clone());
    let target = if let Some(month_name) = captures.get(1) {
        let month = MONTHS
            .iter()
            .position(|month| *month == month_name.as_str().to_lowercase())?;
        let day: i8 = captures.get(2)?.as_str().parse().ok()?;
        if !(1..=31).contains(&day) {
            return None;
        }
        let month = i8::try_from(month + 1).ok()?;
        let this_year = zoned_at(
            &zone,
            jiff::civil::Date::new(today.year(), month, day).ok()?,
            hour,
            minute,
        )?;
        if this_year > now_ms {
            this_year
        } else {
            zoned_at(
                &zone,
                jiff::civil::Date::new(today.year().checked_add(1)?, month, day).ok()?,
                hour,
                minute,
            )?
        }
    } else {
        let this_day = zoned_at(&zone, today.date(), hour, minute)?;
        if this_day > now_ms {
            this_day
        } else {
            zoned_at(&zone, today.date().tomorrow().ok()?, hour, minute)?
        }
    };
    Some(clamp_ms((target - now_ms) as f64))
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_700_000_000_000;

    #[test]
    fn relative_prose_sums_every_part() {
        let reset = usage_limit_reset("usage limit reached, try again in 2h 30m", NOW);
        assert_eq!(
            reset,
            Some(UsageLimitReset {
                ms: 9_000_000,
                kind: ResetKind::Relative
            })
        );
        assert_eq!(
            usage_limit_reset_ms("resets in 45 minutes", NOW),
            Some(2_700_000)
        );
    }

    #[test]
    fn json_fields_name_a_moment_and_the_latest_wins() {
        let text = r#"429 {"error":{"resets_at":1700000060,"nested":{"resets_at":1700000120}},"reset_after_seconds":30}"#;
        assert_eq!(usage_limit_reset_ms(text, NOW), Some(120_000));
        assert_eq!(usage_limit_reset_ms(r#"{"resets_at":1}"#, NOW), Some(0));
        assert_eq!(
            usage_limit_reset_ms(r#"{"resets_at":"2023-11-14T22:15:00Z"}"#, NOW),
            Some(100_000)
        );
    }

    #[test]
    fn clock_prose_in_a_named_zone_resolves_to_the_next_occurrence() {
        // 2023-11-14T22:13:20Z is 2023-11-14 22:13 UTC.
        assert_eq!(
            usage_limit_reset_ms("resets 11pm (UTC)", NOW),
            Some(2_800_000)
        );
        // Already past today in UTC: tomorrow.
        assert_eq!(
            usage_limit_reset_ms("resets 10pm (UTC)", NOW),
            Some(86_400_000 - 800_000)
        );
        assert_eq!(
            usage_limit_reset_ms("resets Nov 15, 12am (UTC)", NOW),
            Some(6_400_000)
        );
    }

    #[test]
    fn text_without_a_well_formed_reset_is_none() {
        assert_eq!(usage_limit_reset_ms("quota exceeded", NOW), None);
        assert_eq!(usage_limit_reset_ms("resets 13pm (UTC)", NOW), None);
        assert_eq!(usage_limit_reset_ms("resets 9am (Not/AZone)", NOW), None);
    }
}
