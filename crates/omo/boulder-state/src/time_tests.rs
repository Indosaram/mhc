use pretty_assertions::assert_eq;

use super::{format_iso_millis, parse_iso_to_millis};

#[test]
fn epoch_formats_as_iso_utc() {
    // given / when
    let formatted = format_iso_millis(0);

    // then
    assert_eq!(formatted, "1970-01-01T00:00:00.000Z");
}

#[test]
fn billion_seconds_formats_as_iso_utc() {
    // given
    let millis = 1_000_000_000_000;

    // when
    let formatted = format_iso_millis(millis);

    // then
    assert_eq!(formatted, "2001-09-09T01:46:40.000Z");
}

#[test]
fn leap_day_formats_as_iso_utc() {
    // given
    let millis = 1_709_164_800_000;

    // when
    let formatted = format_iso_millis(millis);

    // then
    assert_eq!(formatted, "2024-02-29T00:00:00.000Z");
}

#[test]
fn format_and_parse_round_trip() {
    // given
    let samples = [
        0,
        1_000_000_000_000,
        1_709_164_800_000,
        1_780_621_200_123,
        -86_400_000,
    ];

    // when / then
    for millis in samples {
        assert_eq!(
            parse_iso_to_millis(&format_iso_millis(millis)),
            Some(millis)
        );
    }
}

#[test]
fn date_only_input_parses_as_midnight_utc() {
    // given / when
    let millis = parse_iso_to_millis("1970-01-01");

    // then
    assert_eq!(millis, Some(0));
}

#[test]
fn numeric_offset_shifts_the_instant() {
    // given / when / then
    assert_eq!(parse_iso_to_millis("1970-01-01T01:00:00+01:00"), Some(0));
    assert_eq!(parse_iso_to_millis("1969-12-31T19:00:00-05:00"), Some(0));
    assert_eq!(parse_iso_to_millis("1970-01-01T01:00:00+0100"), Some(0));
}

#[test]
fn non_dates_and_impossible_components_are_rejected() {
    // given / when / then
    assert_eq!(parse_iso_to_millis("not-a-date"), None);
    assert_eq!(parse_iso_to_millis("still-not-a-date"), None);
    assert_eq!(parse_iso_to_millis(""), None);
    assert_eq!(parse_iso_to_millis("2026-02-30T00:00:00Z"), None);
    assert_eq!(parse_iso_to_millis("2026-13-01T00:00:00Z"), None);
    assert_eq!(parse_iso_to_millis("2026-06-05T25:00:00Z"), None);
}
