use pretty_assertions::assert_eq;

use super::*;

#[test]
fn test_format_rfc3339_when_epoch_then_it_is_the_unix_epoch_string() {
    assert_eq!(format_rfc3339_millis(0), "1970-01-01T00:00:00.000Z");
}

#[test]
fn test_format_rfc3339_when_known_instant_then_it_matches_the_iso_string() {
    assert_eq!(
        format_rfc3339_millis(946_684_800_000),
        "2000-01-01T00:00:00.000Z"
    );
    assert_eq!(
        format_rfc3339_millis(1_790_553_600_000 + 45_296_789),
        "2026-09-28T12:34:56.789Z"
    );
}

#[test]
fn test_parse_rfc3339_when_iso_string_then_it_round_trips_through_format() {
    for millis in [
        0,
        946_684_800_000,
        1_790_553_645_789,
        1_700_000_000_001,
        -86_400_000,
    ] {
        let formatted = format_rfc3339_millis(millis);
        assert_eq!(parse_rfc3339(&formatted), Some(millis), "{formatted}");
    }
}

#[test]
fn test_parse_rfc3339_when_offset_is_given_then_it_normalizes_to_utc() {
    assert_eq!(
        parse_rfc3339("2026-01-01T01:00:00+01:00"),
        parse_rfc3339("2026-01-01T00:00:00Z")
    );
    assert_eq!(
        parse_rfc3339("2025-12-31T19:00:00-05:00"),
        parse_rfc3339("2026-01-01T00:00:00Z")
    );
}

#[test]
fn test_parse_rfc3339_when_input_is_not_a_timestamp_then_it_is_rejected() {
    assert_eq!(parse_rfc3339("not a date"), None);
    assert_eq!(parse_rfc3339(""), None);
    assert_eq!(parse_rfc3339("2026-13-01T00:00:00Z"), None);
    assert_eq!(parse_rfc3339("2026-01-01T25:00:00Z"), None);
}

#[test]
fn test_parse_rfc3339_when_date_only_then_it_is_midnight_utc() {
    assert_eq!(parse_rfc3339("2000-01-01"), Some(946_684_800_000));
    assert_eq!(parse_rfc3339("2000-01-01T00:00:00"), Some(946_684_800_000));
}
