use super::*;
use serde_json::json;

#[test]
fn accepts_canonical_records_when_strings_contain_spaces_and_proto_keys() {
    let input = br#"{"__proto__":{"x":1},"name":"My Mac","t":"omo-sync/v1/record"}"#;
    let parsed = require_canonical_json(input).expect("canonical record");
    assert_eq!(parsed["__proto__"]["x"], json!(1));
    assert_eq!(parsed["__proto__"]["x"].as_u64(), Some(1));
    assert_eq!(canonicalize_json(&parsed).expect("serialize record"), input);
}

#[test]
fn refuses_noncanonical_input_when_each_violation_is_present() {
    let cases = [
        (r#"{ "t":"record"}"#, Violation::Whitespace),
        (r#"{"z":1,"a":2}"#, Violation::Order),
        (r#"{"a":1,"a":2}"#, Violation::DuplicateKey),
        (r#"{"t":1,"\u0074":2}"#, Violation::DuplicateKey),
        (r#"{"\u0074":1}"#, Violation::Escape),
        (r#"{"t":{"a":1,"a":2}}"#, Violation::DuplicateKey),
        (r#"{"n":1.0}"#, Violation::Number),
        (r#"{"n":1E30}"#, Violation::Number),
        (r#"{"n":2e-3}"#, Violation::Number),
        (r#"{"n":1e999}"#, Violation::Number),
        (r#"{"n":-0}"#, Violation::Number),
        (r#"{"n":9007199254740993}"#, Violation::Number),
        (r#"{"t":"sync\/record"}"#, Violation::Escape),
        (r#"{"t":"\u000F"}"#, Violation::Escape),
        (r#"{"t":"\ud83d"}"#, Violation::Encoding),
        (r#"{"t":"\ud83d\ude00"}"#, Violation::Escape),
        ("{nope", Violation::Json),
        ("{}x", Violation::Json),
        ("[1,]", Violation::Json),
        ("[1e+]", Violation::Json),
        ("[01]", Violation::Json),
        ("\u{feff}{}", Violation::Json),
    ];
    for (input, violation) in cases {
        let error = require_canonical_json(input.as_bytes()).expect_err(input);
        assert_eq!(error.violation, violation, "{input}");
    }
}

#[test]
fn refuses_encoding_when_bytes_are_not_utf8() {
    let input = [0xff, 0xfe];
    let error = require_canonical_json(&input).expect_err("invalid UTF-8");
    assert_eq!(error.violation, Violation::Encoding);
}

#[test]
fn serializes_numbers_when_ecmascript_cutovers_and_rounding_apply() {
    let input = json!([
        333333333.3333333,
        1e30,
        4.50,
        2e-3,
        1e-27,
        1e-6,
        1e-7,
        1e20,
        1e21,
        -0.0,
        9007199254740993u64
    ]);
    let canonical = canonicalize_json(&input).expect("serialize numbers");
    assert_eq!(canonical, b"[333333333.3333333,1e+30,4.5,0.002,1e-27,0.000001,1e-7,100000000000000000000,1e+21,0,9007199254740992]");
    assert!(require_canonical_json(&canonical).is_ok());
}

#[test]
fn sorts_property_names_when_utf16_differs_from_utf8() {
    let input = json!({"\u{fb33}":7,"\u{1f600}":6,"\u{20ac}":5,"ö":4,"\u{80}":3,"1":2,"\r":1});
    let canonical = canonicalize_json(&input).expect("serialize keys");
    assert_eq!(
        canonical,
        "{\"\\r\":1,\"1\":2,\"\u{80}\":3,\"ö\":4,\"€\":5,\"😀\":6,\"דּ\":7}".as_bytes()
    );
    assert!(require_canonical_json(&canonical).is_ok());
}

#[test]
fn minimizes_escapes_when_serializing_control_characters() {
    let input = json!("\u{0f}\n\t\r\u{08}\u{0c}/\"\\€");
    let canonical = canonicalize_json(&input).expect("serialize string");
    assert_eq!(canonical, "\"\\u000f\\n\\t\\r\\b\\f/\\\"\\\\€\"".as_bytes());
    assert!(require_canonical_json(&canonical).is_ok());
}

#[test]
fn bounds_parser_depth_when_input_exceeds_sixty_four_containers() {
    let accepted = format!("{}{}", "[".repeat(64), "]".repeat(64));
    assert!(require_canonical_json(accepted.as_bytes()).is_ok());
    for depth in [65, 200_000] {
        let input = format!("{}{}", "[".repeat(depth), "]".repeat(depth));
        let error = require_canonical_json(input.as_bytes()).expect_err("depth bound");
        assert_eq!(error.violation, Violation::Json);
    }
}

#[test]
fn bounds_serializer_depth_when_value_exceeds_sixty_four_containers() {
    let mut input = json!([]);
    for _ in 0..63 {
        input = Value::Array(vec![input]);
    }
    assert!(canonicalize_json(&input).is_ok());
    input = Value::Array(vec![input]);
    let error = canonicalize_json(&input).expect_err("depth bound");
    assert_eq!(error.violation, Violation::Json);
}
