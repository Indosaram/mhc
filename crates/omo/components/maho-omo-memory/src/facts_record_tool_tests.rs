use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use tempfile::tempdir;

use super::*;

#[tokio::test]
async fn given_a_one_record_budget_when_calls_race_then_the_count_is_reserved_before_append() {
    let root = tempdir().unwrap();
    let path = root.path().join("extraction.jsonl");
    std::fs::write(&path, "").unwrap();

    let failures = Arc::new(Mutex::new(Vec::new()));
    let failures_cb = Arc::clone(&failures);
    let tool = Arc::new(create_facts_record_tool(FactsRecordToolInput {
                exchanges: Vec::new(),
        extraction_path: path.clone(),
        max_records: Some(1),
        max_bytes: None,
        on_failure: Some(Arc::new(move |reason| {
            failures_cb.lock().unwrap().push(reason.to_string());
        })),
        max_calls: None,
        on_call: None,
        state: None,
    }));

    let record = serde_json::json!({
        "scope": "project",
        "text": "fact",
        "date": "2026-08-10"
    });

    let tool1 = Arc::clone(&tool);
    let record1 = record.clone();
    let handle1 = tokio::spawn(async move { tool1.execute("one", &record1) });

    let tool2 = Arc::clone(&tool);
    let record2 = record.clone();
    let handle2 = tokio::spawn(async move { tool2.execute("two", &record2) });

    let (res1, res2) = tokio::join!(handle1, handle2);
    let results = [res1.unwrap(), res2.unwrap()];

    assert_eq!(results.iter().filter(|r| r.is_error).count(), 1);
    assert_eq!(failures.lock().unwrap().len(), 1);
    let lines: Vec<_> = std::fs::read_to_string(&path)
        .unwrap()
        .trim()
        .lines()
        .map(str::to_string)
        .collect();
    assert_eq!(lines.len(), 1);
}

#[tokio::test]
async fn given_an_exact_jsonl_byte_budget_when_two_calls_race_then_only_one_reserves_space_and_any_rejection_invalidates_the_run()
 {
    let root = tempdir().unwrap();
    let path = root.path().join("extraction.jsonl");
    std::fs::write(&path, "").unwrap();

    let record = serde_json::json!({
        "scope": "project",
        "text": "é🌍",
        "date": "2026-08-10"
    });
    let line = format!(
        "{}\n",
        serde_json::to_string(&serde_json::json!({
            "scope": "project",
            "text": "é🌍",
            "date": "2026-08-10",
            "receipt": {"source":"inferred"}
        }))
        .unwrap()
    );
    let line_bytes = line.len();

    let failures = Arc::new(Mutex::new(Vec::new()));
    let failures_cb = Arc::clone(&failures);
    let tool = Arc::new(create_facts_record_tool(FactsRecordToolInput {
                exchanges: Vec::new(),
        extraction_path: path.clone(),
        max_records: None,
        max_bytes: Some(line_bytes),
        on_failure: Some(Arc::new(move |reason| {
            failures_cb.lock().unwrap().push(reason.to_string());
        })),
        max_calls: None,
        on_call: None,
        state: None,
    }));

    let tool1 = Arc::clone(&tool);
    let record1 = record.clone();
    let handle1 = tokio::spawn(async move { tool1.execute("one", &record1) });

    let tool2 = Arc::clone(&tool);
    let record2 = record.clone();
    let handle2 = tokio::spawn(async move { tool2.execute("two", &record2) });

    let (res1, res2) = tokio::join!(handle1, handle2);
    let results = [res1.unwrap(), res2.unwrap()];

    assert_eq!(results.iter().filter(|r| r.is_error).count(), 1);
    assert_eq!(failures.lock().unwrap().len(), 1);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), line);
}

#[test]
fn given_a_bounded_extraction_append_error_when_recording_fails_then_the_owner_is_notified_instead_of_treating_a_prefix_as_success()
 {
    let failures = Arc::new(Mutex::new(Vec::new()));
    let failures_cb = Arc::clone(&failures);
    let root = tempdir().unwrap();

    // Use a directory path instead of a file so append fails with EISDIR
    let tool = create_facts_record_tool(FactsRecordToolInput {
                exchanges: Vec::new(),
        extraction_path: root.path().to_path_buf(),
        max_records: None,
        max_bytes: None,
        on_failure: Some(Arc::new(move |reason| {
            failures_cb.lock().unwrap().push(reason.to_string());
        })),
        max_calls: None,
        on_call: None,
        state: None,
    });

    let res = tool.execute(
        "one",
        &serde_json::json!({
            "scope": "project",
            "text": "fact",
            "date": "2026-08-10"
        }),
    );

    assert!(res.is_error);
    assert_eq!(failures.lock().unwrap().len(), 1);
}

#[test]
fn given_the_registered_record_fact_schema_when_inspected_then_it_is_a_top_level_object_for_provider_tool_contracts()
 {
    let schema = facts_record_parameters();
    assert_eq!(schema["type"], "object");
    assert!(schema.get("anyOf").is_none());
}

#[test]
fn given_malformed_records_when_the_tool_is_called_then_it_returns_errors_and_appends_nothing() {
    let root = tempdir().unwrap();
    let path = root.path().join("extraction.jsonl");
    std::fs::write(&path, "seed\n").unwrap();

    let tool = create_facts_record_tool(FactsRecordToolInput {
                exchanges: Vec::new(),
        extraction_path: path.clone(),
        max_records: None,
        max_bytes: None,
        on_failure: None,
        max_calls: None,
        on_call: None,
        state: None,
    });

    let malformed = [
        serde_json::json!({ "scope": "project", "text": "missing" }),
        serde_json::json!({ "scope": "other", "text": "bad", "date": "2026-08-10" }),
        serde_json::json!({ "scope": "person", "person": "bad", "text": "bad", "date": "2026-08-10" }),
    ];

    let results: Vec<_> = malformed
        .iter()
        .enumerate()
        .map(|(i, rec)| tool.execute(&format!("call-{i}"), rec))
        .collect();

    assert!(results.iter().all(|r| r.is_error));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "seed\n");
}

#[test]
fn given_valid_person_and_project_records_when_recorded_in_order_then_exact_jsonl_lines_are_appended()
 {
    let root = tempdir().unwrap();
    let path = root.path().join("extraction.jsonl");
    std::fs::write(&path, "").unwrap();

    let tool = create_facts_record_tool(FactsRecordToolInput {
                exchanges: Vec::new(),
        extraction_path: path.clone(),
        max_records: None,
        max_bytes: None,
        on_failure: None,
        max_calls: None,
        on_call: None,
        state: None,
    });

    tool.execute(
        "person",
        &serde_json::json!({
            "scope": "person",
            "person": { "name": "Mina", "aliases": ["Min"] },
            "text": "Mina prefers Bun.",
            "date": "2026-08-10"
        }),
    );
    tool.execute(
        "project",
        &serde_json::json!({
            "scope": "project",
            "text": "The project uses Bun.",
            "date": "2026-08-10"
        }),
    );

    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        "{\"scope\":\"person\",\"person\":{\"name\":\"Mina\",\"aliases\":[\"Min\"]},\"text\":\"Mina prefers Bun.\",\"date\":\"2026-08-10\",\"receipt\":{\"source\":\"inferred\"}}\n\
         {\"scope\":\"project\",\"text\":\"The project uses Bun.\",\"date\":\"2026-08-10\",\"receipt\":{\"source\":\"inferred\"}}\n"
    );
}

#[test]
fn given_an_inactive_facts_run_when_a_late_call_arrives_then_it_returns_an_error_without_appending()
{
    let root = tempdir().unwrap();
    let path = root.path().join("extraction.jsonl");
    std::fs::write(&path, "").unwrap();

    let tool = create_facts_record_tool(FactsRecordToolInput {
                exchanges: Vec::new(),
        extraction_path: path.clone(),
        max_records: None,
        max_bytes: None,
        on_failure: None,
        max_calls: None,
        on_call: None,
        state: None,
    });
    tool.deactivate();

    let result = tool.execute(
        "late",
        &serde_json::json!({
            "scope": "project",
            "text": "late",
            "date": "2026-08-10"
        }),
    );

    assert!(result.is_error);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "");
}

#[test]
fn given_a_cancelled_facts_run_state_when_called_then_it_returns_an_error_without_appending() {
    let root = tempdir().unwrap();
    let path = root.path().join("extraction.jsonl");
    std::fs::write(&path, "").unwrap();

    let cancelled = Arc::new(AtomicBool::new(true));
    let tool = create_facts_record_tool(FactsRecordToolInput {
                exchanges: Vec::new(),
        extraction_path: path.clone(),
        max_records: None,
        max_bytes: None,
        on_failure: None,
        max_calls: None,
        on_call: None,
        state: Some(FactsRecordRunState {
            cancelled: Arc::clone(&cancelled),
        }),
    });

    let result = tool.execute(
        "cancelled",
        &serde_json::json!({
            "scope": "project",
            "text": "late",
            "date": "2026-08-10"
        }),
    );

    assert!(result.is_error);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "");
}

#[test]
fn given_a_call_budget_when_rejected_calls_count_then_the_limit_blocks_and_on_call_fires() {
    let root = tempdir().unwrap();
    let path = root.path().join("extraction.jsonl");
    std::fs::write(&path, "").unwrap();
    let seen = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen_cb = Arc::clone(&seen);
    let tool = create_facts_record_tool(FactsRecordToolInput {
                exchanges: Vec::new(),
        extraction_path: path,
        max_records: None,
        max_bytes: None,
        on_failure: None,
        max_calls: Some(2),
        on_call: Some(Arc::new(move || {
            seen_cb.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        })),
        state: None,
    });
    let invalid = serde_json::json!({ "scope": "nope" });
    assert!(tool.execute("1", &invalid).is_error);
    assert!(tool.execute("2", &invalid).is_error);
    let third = tool.execute("3", &invalid);
    assert_eq!(
        third.text,
        "Fact rejected: the facts tool-call limit (2) has been reached"
    );
    assert_eq!(seen.load(std::sync::atomic::Ordering::SeqCst), 2);
}

#[test]
fn repeated_validation_refusal_ends_without_discarding_prior_facts() {
    let root=tempdir().unwrap();let path=root.path().join("facts.jsonl");
    let tool=create_facts_record_tool(FactsRecordToolInput{exchanges:vec![],extraction_path:path.clone(),max_records:None,max_bytes:None,on_failure:None,max_calls:None,on_call:None,state:None});
    assert!(!tool.execute("ok",&serde_json::json!({"scope":"project","text":"kept","date":"2026-10-01"})).is_error);
    let rejected=serde_json::json!({"scope":"wrong","text":"bad","date":"2026-10-01"});
    assert!(tool.execute("one",&rejected).is_error);
    assert!(!tool.ended.load(Ordering::Acquire));
    assert!(tool.execute("two",&rejected).is_error);
    assert!(tool.ended.load(Ordering::Acquire));
    assert_eq!(std::fs::read_to_string(path).unwrap().lines().count(),1);
}
#[test]
fn verified_receipt_can_match_any_adjacent_payload_exchange() {
    let root=tempdir().unwrap();let path=root.path().join("facts.jsonl");
    let tool=create_facts_record_tool(FactsRecordToolInput{exchanges:vec![crate::consent_exchange::ConsentExchange{proposal:"Delete X?".into(),reply:"Yes delete it!".into()},crate::consent_exchange::ConsentExchange{proposal:"Other?".into(),reply:"no".into()}],extraction_path:path.clone(),max_records:None,max_bytes:None,on_failure:None,max_calls:None,on_call:None,state:None});
    assert!(!tool.execute("ok",&serde_json::json!({"scope":"project","text":"Delete X","date":"2026-10-01","source":"user","user_quote":"yes delete it"})).is_error);
    let record:serde_json::Value=serde_json::from_str(std::fs::read_to_string(path).unwrap().trim()).unwrap();
    assert_eq!(record["receipt"]["source"],"user");assert_eq!(record["receipt"]["userQuote"],"Yes delete it!");
}

#[test]
fn payload_exchanges_keep_queue_lead_and_break_on_image_only_user_or_tools(){
 let payload:memory_core::facts::FactsPayload=serde_json::from_value(serde_json::json!({"version":1,"identity":"agent","today":"2026-10-01","knownPeople":[],"primaryHuman":{"slug":"human","aliases":[]},"entries":[{"version":1,"identity":"agent","sessionId":"session","conversationId":"session","enqueuedAt":"2026-10-01T00:00:00Z","range":{"start_message_id":"u1","end_message_id":"u2","start_line":0,"end_snapshot_line":3},"lead":{"kind":"assistant","text":"Delete X?","captured_at":"now","source_line_id":"a","source_message_id":"a"},"entries":[{"kind":"reasoning","text":"ignored","captured_at":"now","source_line_id":"r","source_message_id":"r"},{"kind":"user","text":"ok","captured_at":"now","source_line_id":"u1","source_message_id":"u1"},{"kind":"user","text":"","captured_at":"now","source_line_id":"u2","source_message_id":"u2"}]}]})).unwrap();
 let exchanges=payload_exchanges(&payload);assert_eq!(exchanges.len(),1);assert_eq!(exchanges[0].proposal,"Delete X?");
}
