use pretty_assertions::assert_eq;

use super::*;

const T0: &str = "2026-08-16T00:00:00.000Z";

fn test_record(overrides: impl FnOnce(&mut FactsFailureRecord)) -> FactsFailureRecord {
    let mut rec = FactsFailureRecord {
        conversation_id: "conversation-alpha".to_string(),
        end_message_id: "m2".to_string(),
        end_snapshot_line: 4,
        state: FactsFailureState::Backoff,
        streak: 1,
        first_failure_at: T0.to_string(),
        last_failure_at: T0.to_string(),
        last_reason: FactsFailureReason::ChildExit,
        last_detail: None,
        last_failure_id: "run-a".to_string(),
        next_eligible_at: Some("2026-08-16T00:01:00.000Z".to_string()),
        parked_at: None,
    };
    overrides(&mut rec);
    rec
}

#[test]
fn test_round_trip_well_formed_file() {
    // given
    let record = test_record(|_| {});
    let rendered = render_failures_file(std::slice::from_ref(&record), T0);

    // when
    let parsed = parse_failures_file(&rendered).expect("valid parse");

    // then
    assert_eq!(parsed.version, FACTS_FAILURES_VERSION);
    assert_eq!(parsed.updated_at, T0);
    assert_eq!(parsed.entries, vec![record]);
}

#[test]
fn test_corrupt_json_throws_typed_error() {
    // given
    let raw = "{ not json";

    // when
    let result = parse_failures_file(raw);

    // then
    assert_eq!(result.is_err(), true);
}

#[test]
fn test_unsupported_version_throws_typed_error() {
    // given
    let raw = serde_json::json!({
        "version": 2,
        "updatedAt": T0,
        "entries": []
    })
    .to_string();

    // when
    let result = parse_failures_file(&raw);

    // then
    assert_eq!(result.is_err(), true);
    let err = result.unwrap_err();
    assert_eq!(err.reason.contains("unsupported version"), true);
}

#[test]
fn test_unknown_reason_throws_typed_error() {
    // given
    let mut rec = serde_json::to_value(test_record(|_| {})).expect("json value");
    rec.as_object_mut()
        .unwrap()
        .insert("lastReason".to_string(), serde_json::json!("meteor_strike"));
    let raw = serde_json::json!({
        "version": FACTS_FAILURES_VERSION,
        "updatedAt": T0,
        "entries": [rec]
    })
    .to_string();

    // when
    let result = parse_failures_file(&raw);

    // then
    assert_eq!(result.is_err(), true);
    let err = result.unwrap_err();
    assert_eq!(err.reason.contains("not a known reason"), true);
}

#[test]
fn test_parked_without_parked_at_throws_typed_error() {
    // given
    let mut rec = serde_json::to_value(test_record(|_| {})).expect("json value");
    rec.as_object_mut()
        .unwrap()
        .insert("state".to_string(), serde_json::json!("parked"));
    rec.as_object_mut()
        .unwrap()
        .insert("nextEligibleAt".to_string(), serde_json::Value::Null);
    rec.as_object_mut().unwrap().remove("parkedAt");
    let raw = serde_json::json!({
        "version": FACTS_FAILURES_VERSION,
        "updatedAt": T0,
        "entries": [rec]
    })
    .to_string();

    // when
    let result = parse_failures_file(&raw);

    // then
    assert_eq!(result.is_err(), true);
    let err = result.unwrap_err();
    assert_eq!(
        err.reason,
        "a parked record needs parkedAt and a null nextEligibleAt"
    );
}

#[test]
fn test_non_iso_instant_fields_throw_typed_error() {
    // given
    let corrupt_fields: [(&str, &str); 4] = [
        ("firstFailureAt", "yesterday"),
        ("lastFailureAt", "2026-08-16"),
        ("nextEligibleAt", "soon"),
        ("parkedAt", "not-a-date"),
    ];

    // when / then
    for (field, value) in corrupt_fields {
        let mut rec = if field == "parkedAt" {
            serde_json::to_value(test_record(|r| {
                r.state = FactsFailureState::Parked;
                r.next_eligible_at = None;
            }))
            .expect("json value")
        } else {
            serde_json::to_value(test_record(|_| {})).expect("json value")
        };
        rec.as_object_mut()
            .unwrap()
            .insert(field.to_string(), serde_json::json!(value));
        let raw = serde_json::json!({
            "version": FACTS_FAILURES_VERSION,
            "updatedAt": T0,
            "entries": [rec]
        })
        .to_string();

        let result = parse_failures_file(&raw);
        assert_eq!(result.is_err(), true);
        let err = result.unwrap_err();
        assert_eq!(err.reason.contains("must be an ISO-8601 instant"), true);
    }
}

#[test]
fn test_non_iso_file_updated_at_throws_typed_error() {
    // given
    let raw = serde_json::json!({
        "version": FACTS_FAILURES_VERSION,
        "updatedAt": "not-a-date",
        "entries": []
    })
    .to_string();

    // when
    let result = parse_failures_file(&raw);

    // then
    assert_eq!(result.is_err(), true);
    let err = result.unwrap_err();
    assert_eq!(err.reason.contains("must be an ISO-8601 instant"), true);
}

#[test]
fn test_parked_record_with_null_next_eligible_at() {
    // given
    let parked = test_record(|r| {
        r.state = FactsFailureState::Parked;
        r.streak = 5;
        r.next_eligible_at = None;
        r.parked_at = Some(T0.to_string());
    });

    // when
    let rendered = render_failures_file(std::slice::from_ref(&parked), T0);
    let parsed = parse_failures_file(&rendered).expect("valid parse");

    // then
    assert_eq!(parsed.entries, vec![parked]);
}

#[test]
fn test_render_sorts_records_and_appends_newline() {
    // given
    let entries = vec![
        test_record(|r| {
            r.conversation_id = "b-conversation".to_string();
            r.end_snapshot_line = 4;
            r.end_message_id = "m2".to_string();
        }),
        test_record(|r| {
            r.conversation_id = "a-conversation".to_string();
            r.end_snapshot_line = 9;
            r.end_message_id = "m9".to_string();
        }),
        test_record(|r| {
            r.conversation_id = "a-conversation".to_string();
            r.end_snapshot_line = 4;
            r.end_message_id = "m5".to_string();
        }),
        test_record(|r| {
            r.conversation_id = "a-conversation".to_string();
            r.end_snapshot_line = 4;
            r.end_message_id = "m2".to_string();
        }),
    ];

    // when
    let rendered = render_failures_file(&entries, T0);

    // then
    assert_eq!(rendered.ends_with('\n'), true);
    let parsed = parse_failures_file(&rendered).expect("valid parse");
    let mapped: Vec<(&str, u64, &str)> = parsed
        .entries
        .iter()
        .map(|row| {
            (
                row.conversation_id.as_str(),
                row.end_snapshot_line,
                row.end_message_id.as_str(),
            )
        })
        .collect();
    assert_eq!(
        mapped,
        vec![
            ("a-conversation", 4, "m2"),
            ("a-conversation", 4, "m5"),
            ("a-conversation", 9, "m9"),
            ("b-conversation", 4, "m2"),
        ]
    );
}
