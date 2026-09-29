mod support;

use lsp_core::WorkspaceEditCommitIo;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use support::*;

fn counting_writer(writes: &Arc<AtomicUsize>) -> WorkspaceEditCommitIo {
    let writes = Arc::clone(writes);
    WorkspaceEditCommitIo {
        write_file: Some(Box::new(move |path, content| {
            writes.fetch_add(1, Ordering::SeqCst);
            std::fs::write(path, content).map_err(|error| error.to_string())
        })),
        rename: None,
        remove: None,
    }
}

fn apply_edit_responses(events: &str) -> Vec<Value> {
    read_events(events)
        .into_iter()
        .filter(|event| is_event(event, "clientResponse", "workspace/applyEdit"))
        .collect()
}

#[tokio::test]
async fn server_applied_edit_returned_again_by_rename_reuses_one_write() {
    let context = make_client(json!({
        "renameSteps": [{ "applyEdit": rename_text_edit("before", "after", Some(1), None), "renameResult": "same" }],
        "diagnostics": [diagnostic_json("fresh")],
    }))
    .await;
    let writes = Arc::new(AtomicUsize::new(0));
    context
        .client
        .set_workspace_edit_io(counting_writer(&writes));
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
    assert_eq!(result.apply.total_edits, 1);
    assert_eq!(writes.load(Ordering::SeqCst), 1);
    assert_eq!(read(&context.source), "const after = 1;\n");
    assert_eq!(
        context.client.get_open_document_version(&context.source),
        Some(2)
    );
    assert_eq!(
        context
            .client
            .get_stored_diagnostics(&canonical_uri(&context.source)),
        vec![diagnostic("fresh")]
    );
    let responses = apply_edit_responses(&context.events);
    assert_eq!(responses.len(), 1);
    assert_eq!(responses[0]["result"], json!({ "applied": true }));
    context.stop().await;
}

#[tokio::test]
async fn server_applied_edit_with_null_rename_result_reuses_recorded_result() {
    let context = make_client(json!({
        "renameSteps": [{ "applyEdit": rename_text_edit("before", "after", Some(1), None), "renameResult": "null" }],
    }))
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

    assert_eq!(result.edit, None);
    assert!(result.apply.success);
    assert_eq!(read(&context.source), "const after = 1;\n");
    context.stop().await;
}

#[tokio::test]
async fn two_versioned_server_edits_apply_sequentially_with_synchronized_version() {
    let context = make_client(json!({
        "renameSteps": [
            { "applyEdit": rename_text_edit("before", "first_", Some(1), None), "renameResult": "same" },
            { "applyEdit": rename_text_edit("first_", "second", Some(2), None), "renameResult": "same" },
        ],
    }))
    .await;
    context
        .client
        .open_file(&context.source)
        .await
        .expect("open");

    let first = context
        .client
        .rename(&context.source, 1, 6, "first_", None)
        .await
        .expect("first");
    let second = context
        .client
        .rename(&context.source, 1, 6, "second", None)
        .await
        .expect("second");

    assert!(first.apply.success);
    assert!(second.apply.success);
    assert_eq!(
        context.client.get_open_document_version(&context.source),
        Some(3)
    );
    assert_eq!(read(&context.source), "const second = 1;\n");
    context.stop().await;
}

#[tokio::test]
async fn mismatched_document_version_rejects_mutation_truthfully() {
    let context = make_client(json!({
        "renameSteps": [{ "applyEdit": rename_text_edit("before", "after", Some(9), None), "renameResult": "same" }],
    }))
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

    assert!(!result.apply.success);
    assert!(result.apply.errors.join("\n").contains("document version"));
    assert_eq!(read(&context.source), "const before = 1;\n");
    let response = apply_edit_responses(&context.events).remove(0)["result"].clone();
    assert_eq!(response["applied"], json!(false));
    assert!(
        response["failureReason"]
            .as_str()
            .expect("reason")
            .contains("document version")
    );
    assert_eq!(response["failedChange"], json!(0));
    assert_eq!(response.as_object().expect("object").len(), 3);
    context.stop().await;
}

#[tokio::test]
async fn different_rename_result_reports_conflict_without_second_mutation() {
    let context = make_client(json!({
        "renameSteps": [{
            "applyEdit": rename_text_edit("before", "after", Some(1), None),
            "renameResult": "different",
            "differentEdit": rename_text_edit("before", "other_", Some(1), None),
        }],
    }))
    .await;
    let writes = Arc::new(AtomicUsize::new(0));
    context
        .client
        .set_workspace_edit_io(counting_writer(&writes));
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

    assert!(!result.apply.success);
    assert!(
        result
            .apply
            .errors
            .join("\n")
            .contains("conflicts with server-applied workspace edit")
    );
    assert_eq!(writes.load(Ordering::SeqCst), 1);
    assert_eq!(read(&context.source), "const after = 1;\n");
    context.stop().await;
}

#[tokio::test]
async fn real_commit_io_failure_reports_failure_in_protocol_and_rename_result() {
    let context = make_client(json!({
        "renameSteps": [{ "applyEdit": rename_text_edit("before", "after", Some(1), None), "renameResult": "same" }],
    }))
    .await;
    context.client.set_workspace_edit_io(WorkspaceEditCommitIo {
        write_file: Some(Box::new(|_, _| {
            Err("injected workspace I/O failure".to_string())
        })),
        rename: None,
        remove: None,
    });
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

    assert!(!result.apply.success);
    assert!(
        result
            .apply
            .errors
            .join("\n")
            .contains("I/O failure during text")
    );
    assert_eq!(read(&context.source), "const before = 1;\n");
    let response = apply_edit_responses(&context.events).remove(0)["result"].clone();
    assert_eq!(response["applied"], json!(false));
    assert!(
        response["failureReason"]
            .as_str()
            .expect("reason")
            .contains("I/O failure during text")
    );
    assert_eq!(response["failedChange"], json!(0));
    context.stop().await;
}
