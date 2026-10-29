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
    assert_eq!(parse_iso_to_millis("2026-02-30T00:00:00Z"), parse_iso_to_millis("2026-03-02T00:00:00Z"));
    assert_eq!(parse_iso_to_millis("2026-13-01T00:00:00Z"), None);
    assert_eq!(parse_iso_to_millis("2026-06-05T25:00:00Z"), None);
}

#[test]
fn midnight_rollover_and_rfc_dates_match_date_parse() {
    let expected = parse_iso_to_millis("2026-10-21T00:00:00Z");
    assert_eq!(parse_iso_to_millis("2026-10-20T24:00:00Z"), expected);
    assert_eq!(parse_iso_to_millis("Wed, 21 Oct 2026 00:00:00 GMT"), expected);
    assert_eq!(parse_iso_to_millis("Wed, 21 Oct 2026 02:00:00 +0200"), expected);
    assert_eq!(parse_iso_to_millis("2026-10-20T24:00:00.001Z"), None);
}

#[test]
fn space_separated_date_time_matches_the_t_form() {
    assert_eq!(
        parse_iso_to_millis("2026-10-20 12:00:00Z"),
        parse_iso_to_millis("2026-10-20T12:00:00Z")
    );
    assert_eq!(
        parse_iso_to_millis("2026-10-20 12:00:00"),
        parse_iso_to_millis("2026-10-20T12:00:00")
    );
    assert_eq!(parse_iso_to_millis("2026-10-20 garbage"), None);
}

#[test]
fn unzoned_wall_time_uses_compatible_local_disambiguation() {
    let zone = jiff::tz::TimeZone::get("America/New_York").expect("zone");
    for (wall, instant) in [
        ("2026-01-01T12:00:00Z", "2026-01-01T17:00:00Z"),
        ("2026-03-08T02:30:00Z", "2026-03-08T07:30:00Z"),
        ("2026-11-01T01:30:00Z", "2026-11-01T05:30:00Z"),
    ] {
        assert_eq!(super::local_wall_time_millis(parse_iso_to_millis(wall).expect("wall"), zone.clone()), parse_iso_to_millis(instant));
    }
    let wall = parse_iso_to_millis("2026-10-20T12:00:00Z").expect("wall");
    assert_eq!(parse_iso_to_millis("2026-10-20T12:00:00"), super::local_wall_time_millis(wall, jiff::tz::TimeZone::system()));
}
