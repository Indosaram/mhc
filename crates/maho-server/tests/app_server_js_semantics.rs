use maho_server::app_server::js_semantics::{date_parse_ms, decode_cursor_number, encode_cursor_number, locale_compare, to_locale_lowercase};
use std::cmp::Ordering;

#[test]
fn locale_compare_matches_byte_order_for_canonical_uuids() {
    let ids = [
        "01a0f797-2658-73f3-bf44-bd798fa06162",
        "01a0f7972658-73f3-bf44-bd798fa06162",
        "01a100a4-f001-716d-90d8-b0c86d70387d",
        "ffffffff-ffff-4fff-bfff-ffffffffffff",
        "00000000-0000-4000-8000-000000000001",
    ];
    for left in ids {
        for right in ids {
            assert_eq!(locale_compare(left, right), left.cmp(right), "{left} vs {right}");
        }
    }
}

#[test]
fn locale_compare_orders_punctuation_before_digits_before_letters_and_lowercase_first() {
    assert_eq!(locale_compare("-", "0"), Ordering::Less);
    assert_eq!(locale_compare("0", "a"), Ordering::Less);
    assert_eq!(locale_compare("a", "A"), Ordering::Less);
    assert_eq!(locale_compare("ABC", "abd"), Ordering::Less);
    assert_eq!(locale_compare("same", "same"), Ordering::Equal);
}

#[test]
fn date_parse_accepts_pinned_iso_forms_and_rejects_invalid_input() {
    let expected = 1_577_836_800_000;
    for value in [
        "2020-01-01T00:00:00.000Z",
        "2020-01-01T00:00:00Z",
        "2020-01-01T01:00:00+01:00",
        "2020-01-01T01:00:00+0100",
        "2020-01-01T00:00:00",
        "2020-01-01 00:00:00",
        "2020-01-01",
    ] {
        assert_eq!(date_parse_ms(value), Some(expected), "{value}");
    }
    assert_eq!(date_parse_ms("not a date"), None);
    assert_eq!(date_parse_ms("2020-13-01"), None);
    assert_eq!(date_parse_ms(""), None);
}

#[test]
fn cursor_number_roundtrips_and_matches_parse_int_semantics() {
    assert_eq!(encode_cursor_number(25.0), "MjU=");
    assert_eq!(decode_cursor_number(Some("MjU=")), 25.0);
    assert_eq!(decode_cursor_number(None), 0.0);
    assert_eq!(decode_cursor_number(Some(&encode_cursor_number(1_000_000.0))), 1_000_000.0);
    let prefixed = base64(" 42xyz");
    assert_eq!(decode_cursor_number(Some(&prefixed)), 42.0);
    let negative = base64("-5");
    assert_eq!(decode_cursor_number(Some(&negative)), 0.0);
    let non_numeric = base64("abc");
    assert_eq!(decode_cursor_number(Some(&non_numeric)), 0.0);
    assert_eq!(decode_cursor_number(Some("!!!")), 0.0);
}

#[test]
fn to_locale_lowercase_matches_default_unicode_case_conversion() {
    assert_eq!(to_locale_lowercase("HELLO"), "hello");
    assert_eq!(to_locale_lowercase("Straße"), "straße");
    assert_eq!(to_locale_lowercase("MiXeD 123"), "mixed 123");
    assert_eq!(to_locale_lowercase(""), "");
}

fn base64(value: &str) -> String {
    use base64::{Engine, engine::general_purpose::STANDARD};
    STANDARD.encode(value)
}
