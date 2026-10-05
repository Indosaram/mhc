use maho_server::app_server::{envelope::ClassifiedIncoming, ndjson::*};
use serde_json::json;
#[test]
fn syntax_failure_is_correlated_null_parse_error() {
    assert_eq!(
        parse_ndjson_line("{"),
        NdjsonEmission::ParseError(
            json!({"id":null,"error":{"code":-32700,"message":"Parse error"}})
        )
    );
    assert_eq!(
        parse_ndjson_line("null"),
        NdjsonEmission::Incoming(ClassifiedIncoming::ProtocolInvalid(json!(null)))
    );
}
#[test]
fn serialization_strips_jsonrpc_and_escapes_newlines() {
    let output = serialize_ndjson_message(
        &json!({"jsonrpc":"2.0","id":1,"method":"x","params":"a\nb","extra":true}),
    )
    .unwrap();
    assert_eq!(output.chars().filter(|c| *c == '\n').count(), 1);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&output).unwrap(),
        json!({"id":1,"method":"x","params":"a\nb"})
    );
}
#[test]
fn notification_preserves_timestamp_but_request_drops_it() {
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(
            &serialize_ndjson_message(&json!({"method":"x","emittedAtMs":3})).unwrap()
        )
        .unwrap(),
        json!({"method":"x","emittedAtMs":3})
    );
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(
            &serialize_ndjson_message(&json!({"id":1,"method":"x","emittedAtMs":3})).unwrap()
        )
        .unwrap(),
        json!({"id":1,"method":"x"})
    );
}

#[test]
fn reader_handles_every_utf8_split_and_final_unterminated_line() {
    let input = "{\"method\":\"한국어\",\"params\":\"a\u{2028}b\"}\r\n{\"id\":1,\"result\":null}";
    for split in 0..=input.len() {
        let mut reader = NdjsonReader::default();
        let mut messages = reader.push(&input.as_bytes()[..split]);
        messages.extend(reader.push(&input.as_bytes()[split..]));
        messages.extend(reader.end());
        assert_eq!(messages.len(), 2);
        assert_eq!(
            messages[0],
            NdjsonEmission::Incoming(ClassifiedIncoming::Notification(
                json!({"method":"한국어","params":"a\u{2028}b"})
            ))
        );
        assert!(reader.end().is_none());
    }
}
