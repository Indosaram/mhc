mod support;

use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use std::path::Path;
use support::*;

fn methods(events: &str) -> Vec<String> {
    read_events(events)
        .iter()
        .map(|event| event["method"].as_str().unwrap_or_default().to_string())
        .collect()
}

fn close_precedes_last_open(events: &str) -> bool {
    let methods = methods(events);
    let close = methods
        .iter()
        .position(|method| method == "textDocument/didClose");
    let open = methods
        .iter()
        .rposition(|method| method == "textDocument/didOpen");
    matches!((close, open), (Some(close), Some(open)) if close < open)
}

fn watched_params(events: &str) -> Value {
    read_events(events)
        .into_iter()
        .find(|event| {
            is_event(
                event,
                "clientNotification",
                "workspace/didChangeWatchedFiles",
            )
        })
        .map(|event| event["params"].clone())
        .unwrap_or(Value::Null)
}

#[tokio::test]
async fn open_edited_document_sends_did_change_before_did_save_with_next_version() {
    let context = make_client(json!({
        "renameSteps": [{ "applyEdit": rename_text_edit("before", "after", Some(1), None), "renameResult": "same" }],
    }))
    .await;
    context
        .client
        .open_file(&context.source)
        .await
        .expect("open");

    context
        .client
        .rename(&context.source, 1, 6, "after", None)
        .await
        .expect("rename");

    let notifications: Vec<Value> = read_events(&context.events)
        .into_iter()
        .filter(|event| event["type"] == "clientNotification")
        .collect();
    let change = notifications
        .iter()
        .position(|event| event["method"] == "textDocument/didChange")
        .expect("didChange");
    let save = notifications
        .iter()
        .position(|event| event["method"] == "textDocument/didSave")
        .expect("didSave");
    assert!(save > change);
    assert_eq!(
        notifications[change]["params"]["textDocument"]["version"],
        json!(2)
    );
    context.stop().await;
}

#[tokio::test]
async fn open_file_resource_rename_moves_document_state() {
    let edit = json!({ "documentChanges": [{ "kind": "rename", "oldUri": "file:///placeholder", "newUri": "file:///destination" }] });
    let context =
        make_client(json!({ "renameSteps": [{ "applyEdit": edit, "renameResult": "null" }] }))
            .await;
    context
        .client
        .open_file(&context.source)
        .await
        .expect("open");

    let result = context
        .client
        .rename(&context.source, 1, 6, "after", None)
        .await
        .expect("rename");

    assert!(result.apply.success);
    assert!(!Path::new(&context.source).exists());
    assert_eq!(read(&context.destination), "const before = 1;\n");
    assert_eq!(
        context.client.get_open_document_version(&context.source),
        None
    );
    assert_eq!(
        context
            .client
            .get_open_document_version(&context.destination),
        Some(1)
    );
    assert!(close_precedes_last_open(&context.events));
    context.stop().await;
}

#[tokio::test]
async fn open_file_deletion_closes_document_state() {
    let edit = json!({ "documentChanges": [{ "kind": "delete", "uri": "file:///placeholder" }] });
    let context =
        make_client(json!({ "renameSteps": [{ "applyEdit": edit, "renameResult": "null" }] }))
            .await;
    context
        .client
        .open_file(&context.source)
        .await
        .expect("open");

    let result = context
        .client
        .rename(&context.source, 1, 6, "after", None)
        .await
        .expect("rename");

    assert!(result.apply.success);
    assert!(!Path::new(&context.source).exists());
    assert_eq!(
        context.client.get_open_document_version(&context.source),
        None
    );
    assert!(
        methods(&context.events)
            .iter()
            .any(|method| method == "textDocument/didClose")
    );
    context.stop().await;
}

#[tokio::test]
async fn closed_edited_file_emits_one_changed_watched_file_event() {
    let edit = rename_text_edit("closed", "after_", None, Some("file:///closed"));
    let context =
        make_client(json!({ "renameSteps": [{ "applyEdit": edit, "renameResult": "null" }] }))
            .await;
    context
        .client
        .open_file(&context.source)
        .await
        .expect("open");

    let result = context
        .client
        .rename(&context.source, 1, 6, "after", None)
        .await
        .expect("rename");

    assert!(result.apply.success);
    assert_eq!(read(&context.closed), "const after_ = 1;\n");
    assert_eq!(
        watched_params(&context.events),
        json!({ "changes": [{ "uri": canonical_uri(&context.closed), "type": 2 }] })
    );
    context.stop().await;
}

#[tokio::test]
async fn new_closed_file_create_emits_one_created_watched_file_event() {
    let edit = json!({ "documentChanges": [{ "kind": "create", "uri": "file:///created" }] });
    let context =
        make_client(json!({ "renameSteps": [{ "applyEdit": edit, "renameResult": "null" }] }))
            .await;
    context
        .client
        .open_file(&context.source)
        .await
        .expect("open");

    let result = context
        .client
        .rename(&context.source, 1, 6, "after", None)
        .await
        .expect("rename");

    assert!(result.apply.success);
    assert_eq!(read(&context.created), "");
    assert_eq!(
        watched_params(&context.events),
        json!({ "changes": [{ "uri": canonical_uri(&context.created), "type": 1 }] })
    );
    context.stop().await;
}

#[tokio::test]
async fn open_file_overwritten_by_create_resets_state_via_close_and_open() {
    let edit = json!({ "documentChanges": [{ "kind": "create", "uri": "file:///closed", "options": { "overwrite": true } }] });
    let context =
        make_client(json!({ "renameSteps": [{ "applyEdit": edit, "renameResult": "null" }] }))
            .await;
    context
        .client
        .open_file(&context.source)
        .await
        .expect("open source");
    context
        .client
        .open_file(&context.closed)
        .await
        .expect("open closed");

    let result = context
        .client
        .rename(&context.source, 1, 6, "after", None)
        .await
        .expect("rename");

    assert!(result.apply.success);
    assert_eq!(read(&context.closed), "");
    assert_eq!(
        context.client.get_open_document_version(&context.closed),
        Some(1)
    );
    assert!(close_precedes_last_open(&context.events));
    context.stop().await;
}
