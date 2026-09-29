mod support;

use lsp_core::AbortController;
use lsp_core::WorkspaceEditCommitIo;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use support::*;

fn apply_edit_responses(events: &str) -> Vec<Value> {
    read_events(events)
        .into_iter()
        .filter(|event| is_event(event, "clientResponse", "workspace/applyEdit"))
        .collect()
}

fn is_rename_request(event: &Value) -> bool {
    is_event(event, "clientRequest", "textDocument/rename")
}

fn assert_rejected(result: &Value, reason_fragment: &str) {
    assert_eq!(result["applied"], json!(false), "{result}");
    assert!(
        result["failureReason"]
            .as_str()
            .expect("failureReason")
            .contains(reason_fragment),
        "{result}"
    );
    assert_eq!(result.as_object().expect("object").len(), 2, "{result}");
}

fn counting_writer(
    writes: &Arc<AtomicUsize>,
    on_write: impl Fn() + Send + 'static,
) -> WorkspaceEditCommitIo {
    let writes = Arc::clone(writes);
    WorkspaceEditCommitIo {
        write_file: Some(Box::new(move |path, content| {
            writes.fetch_add(1, Ordering::SeqCst);
            std::fs::write(path, content).map_err(|error| error.to_string())?;
            on_write();
            Ok(())
        })),
        rename: None,
        remove: None,
    }
}

#[tokio::test]
async fn unscoped_server_apply_edit_is_rejected() {
    let context = make_client(
        json!({ "unscopedApplyEdit": rename_text_edit("before", "after", Some(1), None) }),
    )
    .await;

    let responses = wait_for_event_count(
        &context.events,
        |event| is_event(event, "clientResponse", "workspace/applyEdit"),
        1,
    )
    .await;

    assert_rejected(&responses[0]["result"], "active workspace mutation");
    assert_eq!(read(&context.source), "const before = 1;\n");
    context.stop().await;
}

#[tokio::test]
async fn repeated_server_apply_is_rejected_as_already_handled() {
    let context = make_client(json!({
        "renameSteps": [{
            "applyEdit": rename_text_edit("before", "after", Some(1), None), "applyTwice": true, "renameResult": "same",
        }],
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

    assert!(result.apply.success);
    let responses = apply_edit_responses(&context.events);
    assert_eq!(responses.len(), 2);
    assert_eq!(responses[0]["result"], json!({ "applied": true }));
    assert_rejected(&responses[1]["result"], "already handled");
    assert_eq!(read(&context.source), "const after = 1;\n");
    context.stop().await;
}

#[tokio::test]
async fn concurrent_server_applies_commit_once_and_reject_the_other() {
    let context = make_client(json!({
        "renameSteps": [{
            "applyEdit": rename_text_edit("before", "after", Some(1), None),
            "applyConcurrently": true,
            "renameResult": "same",
        }],
    }))
    .await;
    let writes = Arc::new(AtomicUsize::new(0));
    context
        .client
        .set_workspace_edit_io(counting_writer(&writes, || {}));
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
    assert_eq!(writes.load(Ordering::SeqCst), 1);
    let results: Vec<Value> = apply_edit_responses(&context.events)
        .into_iter()
        .map(|response| response["result"].clone())
        .collect();
    assert!(results.contains(&json!({ "applied": true })), "{results:?}");
    let rejection = results
        .iter()
        .find(|result| result.get("failureReason").is_some())
        .expect("rejection");
    assert_eq!(rejection["applied"], json!(false));
    let text = rejection.to_string();
    assert!(
        text.contains("already in progress") || text.contains("already handled"),
        "{text}"
    );
    context.stop().await;
}

#[tokio::test]
async fn held_rename_lease_rejects_second_rename_without_request() {
    let context = make_client(json!({
        "renameSteps": [{
            "renameResult": "edit",
            "renameEdit": rename_text_edit("before", "after", Some(1), None),
            "responseDelayMs": 150,
        }],
    }))
    .await;
    context
        .client
        .open_file(&context.source)
        .await
        .expect("open");
    let first_future = context.client.rename(&context.source, 1, 6, "after", None);
    let second_future = async {
        wait_for_event_count(&context.events, is_rename_request, 1).await;
        context
            .client
            .rename(&context.source, 1, 6, "other_", None)
            .await
    };

    let (first, concurrent) = tokio::join!(first_future, second_future);

    let (first, concurrent) = (first.expect("first"), concurrent.expect("concurrent"));
    assert!(!concurrent.apply.success);
    assert!(
        concurrent
            .apply
            .errors
            .join("\n")
            .contains("workspace mutation is already in progress")
    );
    assert!(first.apply.success);
    let requests: Vec<Value> = read_events(&context.events)
        .into_iter()
        .filter(is_rename_request)
        .collect();
    assert_eq!(requests.len(), 1);
    context.stop().await;
}

#[tokio::test]
async fn direct_rename_edit_without_server_apply_is_applied_once() {
    let context = make_client(json!({
        "renameSteps": [{ "renameResult": "edit", "renameEdit": rename_text_edit("before", "after", Some(1), None) }],
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

    assert!(result.apply.success);
    assert_eq!(read(&context.source), "const after = 1;\n");
    assert_eq!(
        read_events(&context.events)
            .iter()
            .filter(|event| event["method"] == "workspace/applyEdit")
            .count(),
        0
    );
    context.stop().await;
}

#[tokio::test]
async fn cancellation_before_delayed_server_apply_mutates_nothing() {
    let controller = AbortController::new();
    let signal = controller.signal();
    let context = make_client(json!({
        "renameSteps": [{
            "applyEdit": rename_text_edit("before", "after", Some(1), None),
            "applyDelayMs": 100,
            "renameResult": "same",
        }],
    }))
    .await;
    context
        .client
        .open_file(&context.source)
        .await
        .expect("open");
    let rename = context
        .client
        .rename(&context.source, 1, 6, "after", Some(&signal));
    let abort = async {
        wait_for_event_count(&context.events, is_rename_request, 1).await;
        controller.abort();
    };

    let (result, ()) = tokio::join!(rename, abort);

    let error = result.expect_err("aborted rename rejects");
    assert!(
        error.to_string().to_lowercase().contains("cancel"),
        "{error}"
    );
    let cancels = wait_for_event_count(
        &context.events,
        |event| is_event(event, "clientNotification", "$/cancelRequest"),
        1,
    )
    .await;
    assert_eq!(cancels[0]["params"], json!({ "id": 2 }));
    assert_eq!(read(&context.source), "const before = 1;\n");
    context.stop().await;
}

#[tokio::test]
async fn cancellation_after_commit_gate_reports_one_late_abort_mutation() {
    let controller = AbortController::new();
    let signal = controller.signal();
    let context = make_client(json!({
        "renameSteps": [{ "applyEdit": rename_text_edit("before", "after", Some(1), None), "renameResult": "same" }],
    }))
    .await;
    let writes = Arc::new(AtomicUsize::new(0));
    let abort_controller = controller.clone();
    context
        .client
        .set_workspace_edit_io(counting_writer(&writes, move || abort_controller.abort()));
    context
        .client
        .open_file(&context.source)
        .await
        .expect("open");

    let result = context
        .client
        .rename(&context.source, 1, 6, "after", Some(&signal))
        .await
        .expect("rename");

    assert_eq!(
        (
            result.apply.success,
            result.apply.late_abort,
            result.apply.total_edits
        ),
        (true, Some(true), 1)
    );
    assert_eq!(writes.load(Ordering::SeqCst), 1);
    assert_eq!(read(&context.source), "const after = 1;\n");
    context.stop().await;
}

fn initialize_capabilities(events: &str) -> Value {
    read_events(events)
        .into_iter()
        .find(|event| is_event(event, "clientRequest", "initialize"))
        .map(|event| event["params"]["capabilities"].clone())
        .unwrap_or_else(|| json!({}))
}

#[tokio::test]
async fn lease_backed_and_bare_clients_advertise_apply_edit_honestly() {
    let active = make_client(json!({})).await;
    let (bare, inactive) = make_bare_connection(json!({})).await;

    let active_capabilities = initialize_capabilities(&active.events);
    let inactive_capabilities = initialize_capabilities(&inactive.events);

    assert_eq!(active_capabilities["workspace"]["applyEdit"], json!(true));
    assert_eq!(inactive_capabilities["workspace"].get("applyEdit"), None);
    assert_eq!(
        active_capabilities["textDocument"]["rename"].get("honorsChangeAnnotations"),
        None
    );
    assert_eq!(
        active_capabilities["workspace"]["workspaceEdit"].get("changeAnnotationSupport"),
        None
    );
    active.stop().await;
    bare.stop().await;
}
