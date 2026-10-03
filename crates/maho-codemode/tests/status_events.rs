use maho_codemode::tool::status_events::{STATUS_EVENT_HISTORY_LIMIT, upsert_status_event};
use serde_json::json;

#[test]
fn distinct_agents_append_in_first_seen_order() {
    let mut events = Vec::new();
    upsert_status_event(&mut events, json!({"op":"agent", "id":"a1", "status":"running"}));
    upsert_status_event(&mut events, json!({"op":"agent", "id":"a2", "status":"running"}));
    assert_eq!(events.iter().map(|event| event["id"].as_str().unwrap()).collect::<Vec<_>>(), ["a1", "a2"]);
}

#[test]
fn agent_progress_replaces_without_reordering() {
    let mut events = vec![json!({"op":"agent", "id":"a1", "status":"running"}), json!({"op":"read", "path":"/tmp/x"})];
    upsert_status_event(&mut events, json!({"op":"agent", "id":"a1", "status":"completed"}));
    assert_eq!(events, [json!({"op":"agent", "id":"a1", "status":"completed"}), json!({"op":"read", "path":"/tmp/x"})]);
}

#[test]
fn duplicate_operations_and_agents_without_ids_append() {
    let mut events = Vec::new();
    for event in [json!({"op":"read", "path":"/tmp/x"}), json!({"op":"read", "path":"/tmp/x"}), json!({"op":"agent", "status":"missing-id"}), json!({"op":"agent", "status":"missing-id"})] {
        upsert_status_event(&mut events, event);
    }
    assert_eq!(events.len(), 4);
    assert_eq!(events[0], events[1]);
    assert_eq!(events[2], events[3]);
}

#[test]
fn history_bounds_accumulate_omitted_count() {
    let mut events = Vec::new();
    for index in 0..STATUS_EVENT_HISTORY_LIMIT + 25 {
        upsert_status_event(&mut events, json!({"op":"read", "path":format!("/tmp/file-{index}.txt"), "preview":"x".repeat(500)}));
    }
    assert_eq!(events.len(), STATUS_EVENT_HISTORY_LIMIT);
    assert_eq!(events[0]["op"], "status-events-omitted");
    assert_eq!(events[0]["count"].as_f64(), Some(26.0));
    assert_eq!(events[1]["path"], "/tmp/file-26.txt");
    assert_eq!(events.last().unwrap()["path"], "/tmp/file-124.txt");
}

#[test]
fn event_101_reserves_omission_marker() {
    let mut events = Vec::new();
    for index in 0..=STATUS_EVENT_HISTORY_LIMIT {
        upsert_status_event(&mut events, json!({"op":"read", "path":format!("/tmp/file-{index}.txt")}));
    }
    assert_eq!(events.len(), STATUS_EVENT_HISTORY_LIMIT);
    assert_eq!(events[0], json!({"op":"status-events-omitted", "count":2}));
    assert_eq!(events[1]["path"], "/tmp/file-2.txt");
    assert_eq!(events.last().unwrap()["path"], "/tmp/file-100.txt");
}
