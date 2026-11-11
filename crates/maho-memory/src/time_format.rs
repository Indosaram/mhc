//! RFC 3339 timestamps as Unix milliseconds.
//!
//! Memory files, ledgers and lock records all store instants as `toISOString()`-shaped text, so the
//! same convert-to-millis step was written in four modules before this one existed. Keeping one copy
//! matters because these values are compared for expiry and backoff ordering: a parser that accepted
//! a slightly different shape in one module would silently place an entry on the wrong side of a
//! deadline while every other module agreed.

use chrono::SecondsFormat;

/// Milliseconds since the epoch, or `None` when the text is not RFC 3339.
pub fn parse_millis(value: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|time| time.timestamp_millis())
}

/// `2026-11-11T10:00:00.000Z` from milliseconds since the epoch.
pub fn format_millis(millis: i64) -> Option<String> {
    chrono::DateTime::from_timestamp_millis(millis)
        .map(|time| time.to_rfc3339_opts(SecondsFormat::Millis, true))
}

/// As [`format_millis`], for callers that render into text unconditionally.
///
/// An out-of-range value renders as the empty string rather than a placeholder date: a blank
/// timestamp is visibly wrong, while a plausible-looking wrong instant would be trusted.
pub fn format_millis_or_empty(millis: i64) -> String {
    format_millis(millis).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_instant_round_trips_through_milliseconds() {
        let text = "2026-11-11T10:20:30.400Z";
        let millis = parse_millis(text).expect("parses");
        assert_eq!(format_millis(millis).as_deref(), Some(text));
    }

    #[test]
    fn a_whole_second_gains_an_explicit_millisecond_field() {
        // `toISOString()` always carries milliseconds, and stored text is compared as strings.
        let millis = parse_millis("2026-11-11T10:20:30Z").expect("parses");
        assert_eq!(
            format_millis(millis).as_deref(),
            Some("2026-11-11T10:20:30.000Z")
        );
    }

    #[test]
    fn an_offset_is_normalised_to_utc() {
        let millis = parse_millis("2026-11-11T12:20:30.000+02:00").expect("parses");
        assert_eq!(
            format_millis(millis).as_deref(),
            Some("2026-11-11T10:20:30.000Z")
        );
    }

    #[test]
    fn text_that_is_not_an_instant_is_rejected() {
        for text in ["", "2026-11-11", "yesterday", "2026-13-45T00:00:00Z"] {
            assert_eq!(parse_millis(text), None, "{text:?}");
        }
    }

    #[test]
    fn an_unrepresentable_value_renders_empty_rather_than_a_plausible_date() {
        assert_eq!(format_millis_or_empty(i64::MAX), "");
    }
}
