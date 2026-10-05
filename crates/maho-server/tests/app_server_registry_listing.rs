use maho_server::app_server::registry_listing::{compare_threads,decode_cursor,encode_cursor};
use serde_json::json;

#[test]
fn cursor_decoding_preserves_node_decimal_prefix_and_urlsafe_base64() {
    assert_eq!(decode_cursor(None),0);
    assert_eq!(decode_cursor(Some(&encode_cursor(123))),123);
    assert_eq!(decode_cursor(Some("MTJqdW5r")),12);
    assert_eq!(decode_cursor(Some("LTE=")),0);
    assert_eq!(decode_cursor(Some("not-a-cursor")),0);
    assert_eq!(decode_cursor(Some("MTIz\n")),123);
}

#[test]
fn threads_sort_newest_first_then_by_id() {
    let mut threads = [json!({"id":"b","updatedAt":"2020-01-01T00:00:00.000Z"}),json!({"id":"a","updatedAt":"2020-01-01T00:00:00.000Z"}),json!({"id":"c","updatedAt":"2021-01-01T00:00:00.000Z"})];
    threads.sort_by(compare_threads);
    assert_eq!(threads.map(|thread|thread["id"].clone()),[json!("c"),json!("a"),json!("b")]);
}

#[test]
fn threads_sort_by_parsed_updated_at_and_locale_id_tiebreak() {
    let mut threads = [json!({"id":"offset","updatedAt":"2020-01-01T00:00:00+05:00"}),json!({"id":"zulu","updatedAt":"2019-12-31T20:00:00.000Z"})];
    threads.sort_by(compare_threads);
    assert_eq!(threads.map(|thread|thread["id"].clone()),[json!("zulu"),json!("offset")]);
    let mut tied = [json!({"id":"b","updatedAt":"2020-01-01"}),json!({"id":"a","updatedAt":"2020-01-01"})];
    tied.sort_by(compare_threads);
    assert_eq!(tied.map(|thread|thread["id"].clone()),[json!("a"),json!("b")]);
}
