use pretty_assertions::assert_eq;

use super::*;

#[test]
fn given_valid_purpose_when_created_then_record_is_populated() {
    // #given
    let purpose = "memory-write";

    // #when
    let record = create_lock_record(
        purpose,
        CreateLockRecordOptions {
            run_id: Some("run-42".to_string()),
        },
    )
    .unwrap();

    // #then
    assert_eq!(record.pid, std::process::id());
    assert!(!record.process_start.is_empty());
    assert!(!record.hostname.is_empty());
    assert!(!record.nonce.is_empty());
    assert!(!record.created_at.is_empty());
    assert_eq!(record.purpose, "memory-write");
    assert_eq!(record.run_id, Some("run-42".to_string()));
}

#[test]
fn given_empty_purpose_when_created_then_fails() {
    // #given
    let purpose = "";

    // #when
    let result = create_lock_record(purpose, CreateLockRecordOptions::default());

    // #then
    assert_eq!(result, Err(LockRecordError::EmptyPurpose));
}

#[test]
fn given_valid_serialized_record_when_parsed_then_matches() {
    // #given
    let original = LockRecord {
        pid: 12345,
        process_start: "ps-lstart:Sun Sep 27 00:00:00 2026".to_string(),
        hostname: "test-host".to_string(),
        nonce: "test-nonce-1234".to_string(),
        created_at: "2026-09-28T12:00:00.000Z".to_string(),
        purpose: "reflection-scheduler".to_string(),
        run_id: Some("run-abc".to_string()),
        git_activity_lock: Some(true),
    };
    let json = serde_json::to_string(&original).unwrap();

    // #when
    let parsed = parse_lock_record(&json).unwrap();

    // #then
    assert_eq!(parsed, original);
}

#[test]
fn given_invalid_json_or_schema_when_parsed_then_returns_none() {
    // #given / #when / #then
    assert!(parse_lock_record("not-json").is_none());
    assert!(parse_lock_record("[]").is_none());
    assert!(parse_lock_record(r#"{"pid": 0}"#).is_none());
    assert!(parse_lock_record(r#"{"pid": -10}"#).is_none());
    assert!(parse_lock_record(
        r#"{"pid":1,"process_start":"","hostname":"h","nonce":"n","created_at":"2026-09-28T12:00:00Z","purpose":"p"}"#
    )
    .is_none());
    assert!(parse_lock_record(
        r#"{"pid":1,"process_start":"s","hostname":"","nonce":"n","created_at":"2026-09-28T12:00:00Z","purpose":"p"}"#
    )
    .is_none());
    assert!(parse_lock_record(
        r#"{"pid":1,"process_start":"s","hostname":"h","nonce":"","created_at":"2026-09-28T12:00:00Z","purpose":"p"}"#
    )
    .is_none());
    assert!(parse_lock_record(
        r#"{"pid":1,"process_start":"s","hostname":"h","nonce":"n","created_at":"invalid-date","purpose":"p"}"#
    )
    .is_none());
    assert!(parse_lock_record(
        r#"{"pid":1,"process_start":"s","hostname":"h","nonce":"n","created_at":"2026-09-28T12:00:00Z","purpose":""}"#
    )
    .is_none());
    assert!(parse_lock_record(
        r#"{"pid":1,"process_start":"s","hostname":"h","nonce":"n","created_at":"2026-09-28T12:00:00Z","purpose":"p","run_id":""}"#
    )
    .is_none());
    assert!(parse_lock_record(
        r#"{"pid":1,"process_start":"s","hostname":"h","nonce":"n","created_at":"2026-09-28T12:00:00Z","purpose":"p","run_id":null}"#
    )
    .is_none());
}

#[test]
fn activity_marker_is_emitted_only_when_supported_and_only_true_is_parsed() {
    let record = create_lock_record("test", Default::default()).expect("record");
    assert_eq!(
        record.git_activity_lock,
        super::super::os_file_lock::os_file_lock_available().then_some(true)
    );
    let mut value = serde_json::to_value(record).expect("json");
    for invalid in [
        serde_json::Value::Bool(false),
        serde_json::Value::Null,
        serde_json::json!("true"),
    ] {
        value["git_activity_lock"] = invalid;
        assert_eq!(
            parse_lock_record(&value.to_string())
                .expect("record")
                .git_activity_lock,
            None
        );
    }
    value["git_activity_lock"] = serde_json::Value::Bool(true);
    assert_eq!(
        parse_lock_record(&value.to_string())
            .expect("record")
            .git_activity_lock,
        Some(true)
    );
}
