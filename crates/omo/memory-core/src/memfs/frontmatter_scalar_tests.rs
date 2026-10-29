use super::*;

#[test]
fn split_frontmatter_handles_lf_and_crlf_and_requires_closing_delimiter() {
    assert_eq!(
        split_frontmatter("---\ndescription: X\n---\nbody\n"),
        Some(("description: X".to_string(), "body\n".to_string()))
    );
    assert_eq!(
        split_frontmatter("---\r\ndescription: X\r\n---\r\nbody"),
        Some(("description: X".to_string(), "body".to_string()))
    );
    assert_eq!(split_frontmatter("no frontmatter"), None);
    assert_eq!(split_frontmatter("---\ndescription: X\n"), None);
}

#[test]
fn plain_safe_scalars_reject_yaml_specials_and_indicators() {
    assert!(is_plain_safe_scalar("hello world"));
    assert!(is_plain_safe_scalar("Ada Lovelace"));
    assert!(!is_plain_safe_scalar(""));
    assert!(!is_plain_safe_scalar("true"));
    assert!(!is_plain_safe_scalar("null"));
    assert!(!is_plain_safe_scalar("~"));
    assert!(!is_plain_safe_scalar("42"));
    assert!(!is_plain_safe_scalar("1.5"));
    assert!(!is_plain_safe_scalar("0x1f"));
    assert!(!is_plain_safe_scalar(" lead"));
    assert!(!is_plain_safe_scalar("trail "));
    assert!(!is_plain_safe_scalar("-lead"));
    assert!(!is_plain_safe_scalar("#lead"));
    assert!(!is_plain_safe_scalar("a: b"));
    assert!(!is_plain_safe_scalar("end:"));
    assert!(!is_plain_safe_scalar("a #b"));
}

#[test]
fn quoted_scalars_decode_in_both_subset_forms() {
    assert_eq!(decode_quoted_scalar("\"a\\nb\"").as_deref(), Some("a\nb"));
    assert_eq!(decode_quoted_scalar("\"\\u0041\"").as_deref(), Some("A"));
    assert_eq!(decode_quoted_scalar("'it''s'").as_deref(), Some("it's"));
    assert_eq!(decode_quoted_scalar("'plain'").as_deref(), Some("plain"));
    assert_eq!(decode_quoted_scalar("\"unterminated"), None);
    assert_eq!(decode_quoted_scalar("'bad'quote'"), None);
    assert_eq!(decode_quoted_scalar("plain"), None);
}

#[test]
fn string_arrays_decode_only_for_json_arrays_of_strings() {
    assert_eq!(
        decode_string_array("[\"a\",\"b\"]"),
        Some(vec!["a".to_string(), "b".to_string()])
    );
    assert_eq!(decode_string_array("[]"), Some(Vec::new()));
    assert_eq!(decode_string_array("[1]"), None);
    assert_eq!(decode_string_array("[\"a\","), None);
    assert_eq!(decode_string_array("\"a\""), None);
}

#[test]
fn rendering_quotes_only_when_a_plain_scalar_would_not_round_trip() {
    assert_eq!(render_string_scalar("hello"), "hello");
    assert_eq!(render_string_scalar("true"), "\"true\"");
    assert_eq!(render_string_scalar("a: b"), "\"a: b\"");
    assert_eq!(render_raw_scalar("true"), "true");
    assert_eq!(render_raw_scalar("a: b"), "\"a: b\"");
}

#[test]
fn canonical_sources_accept_specials_numbers_and_json_forms() {
    assert!(is_canonical_scalar_source("hello"));
    assert!(is_canonical_scalar_source("true"));
    assert!(is_canonical_scalar_source("42"));
    assert!(is_canonical_scalar_source("\"quoted\""));
    assert!(is_canonical_scalar_source("[\"a\"]"));
    assert!(!is_canonical_scalar_source("a: b"));
    assert!(!is_canonical_scalar_source("\"unterminated"));
}

#[test]
fn scalar_source_decodes_quotes_and_preserves_plain_text() {
    assert_eq!(decode_scalar_source("\"a b\""), "a b");
    assert_eq!(decode_scalar_source("plain"), "plain");
}
