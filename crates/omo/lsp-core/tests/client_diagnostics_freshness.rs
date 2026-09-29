mod support;

use lsp_core::LspClientOptions;
use lsp_core::LspClientTimeoutOptions;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use std::time::Instant;
use support::*;

fn options(freshness_ms: u64, quiescence_ms: f64) -> LspClientOptions {
    LspClientOptions {
        diagnostics_freshness_timeout_ms: Some(freshness_ms),
        versionless_publish_quiescence_ms: Some(quiescence_ms),
        ..default_options()
    }
}

fn pull_capabilities() -> Value {
    json!({ "diagnosticProvider": { "interFileDependencies": false, "workspaceDiagnostics": false } })
}

fn is_barrier_response(event: &Value) -> bool {
    is_event(event, "clientResponse", "workspace/configuration")
}

fn is_pull_request(event: &Value) -> bool {
    is_event(event, "clientRequest", "textDocument/diagnostic")
}

fn transient_kind(result: &lsp_core::LspDiagnosticsResult) -> Option<&'static str> {
    result.transient_error.as_ref().map(|error| error.kind)
}

async fn change_and_await_barrier(context: &WorkspaceEditTestContext, content: &str) {
    std::fs::write(&context.source, content).expect("write");
    context
        .client
        .open_file(&context.source)
        .await
        .expect("reopen");
    assert_eq!(
        wait_for_event_count(&context.events, is_barrier_response, 1)
            .await
            .len(),
        1
    );
}

#[tokio::test]
async fn post_change_versionless_publish_waits_for_quiescence_and_is_returned() {
    let context = make_client_with(
        json!({ "publishDiagnostics": [{
            "trigger": "didChange", "diagnostics": [diagnostic_json("post-generation-versionless")],
            "awaitClientDelivery": true,
        }] }),
        options(80, 20.0),
    )
    .await;
    context
        .client
        .open_file(&context.source)
        .await
        .expect("open");
    let started = Instant::now();
    change_and_await_barrier(&context, "const after = 1;\n").await;

    let result = context
        .client
        .diagnostics(&context.source, None)
        .await
        .expect("diagnostics");

    assert_eq!(
        result.items,
        vec![diagnostic("post-generation-versionless")]
    );
    assert!(started.elapsed().as_millis() >= 15);
    context.stop().await;
}

#[tokio::test]
async fn pre_generation_versionless_publish_is_ignored_for_newer_version() {
    let context = make_client_with(
        json!({ "publishDiagnostics": [{
            "trigger": "didOpen", "diagnostics": [diagnostic_json("pre-generation-versionless")],
        }] }),
        options(300, 5.0),
    )
    .await;
    context
        .client
        .open_file(&context.source)
        .await
        .expect("open");
    let initial = context
        .client
        .diagnostics(&context.source, None)
        .await
        .expect("initial");
    assert_eq!(
        initial.items,
        vec![diagnostic("pre-generation-versionless")]
    );
    std::fs::write(&context.source, "const after = 1;\n").expect("write");
    context
        .client
        .open_file(&context.source)
        .await
        .expect("reopen");

    let result = context
        .client
        .diagnostics(&context.source, None)
        .await
        .expect("diagnostics");

    assert_eq!(result.items, vec![]);
    assert_eq!(transient_kind(&result), Some("freshness_timeout"));
    context.stop().await;
}

#[tokio::test]
async fn stale_and_future_publish_versions_do_not_satisfy_current_version() {
    for (version, label, content) in [
        (1, "stale", "const stale = 1;\n"),
        (3, "future", "const future = 1;\n"),
    ] {
        let context = make_client_with(
            json!({ "publishDiagnostics": [{
                "trigger": "didChange", "version": version,
                "diagnostics": [diagnostic_json(label)], "awaitClientDelivery": true,
            }] }),
            options(100, 5.0),
        )
        .await;
        context
            .client
            .open_file(&context.source)
            .await
            .expect("open");
        change_and_await_barrier(&context, content).await;

        let result = context
            .client
            .diagnostics(&context.source, None)
            .await
            .expect("diagnostics");

        assert_eq!(result.items, vec![], "{label}");
        assert_eq!(
            transient_kind(&result),
            Some("freshness_timeout"),
            "{label}"
        );
        context.stop().await;
    }
}

#[tokio::test]
async fn pull_overtaken_by_local_change_restarts_and_resolves_current_version() {
    let context = make_client_with(
        json!({
            "capabilities": pull_capabilities(),
            "diagnosticResponses": [
                { "delayMs": 200, "report": { "kind": "full", "resultId": "v1", "items": [diagnostic_json("pull-stale")] } },
                { "report": { "kind": "full", "resultId": "v2", "items": [diagnostic_json("pull-fresh")] } },
            ],
        }),
        options(800, 5.0),
    )
    .await;
    context
        .client
        .open_file(&context.source)
        .await
        .expect("open");
    let pending = context.client.diagnostics(&context.source, None);
    let change = async {
        wait_for_event_count(&context.events, is_pull_request, 1).await;
        std::fs::write(&context.source, "const pull_fresh = 1;\n").expect("write");
        context
            .client
            .open_file(&context.source)
            .await
            .expect("reopen");
    };

    let (result, ()) = tokio::join!(pending, change);

    assert_eq!(
        result.expect("diagnostics").items,
        vec![diagnostic("pull-fresh")]
    );
    assert_eq!(
        read_events(&context.events)
            .into_iter()
            .filter(is_pull_request)
            .count(),
        2
    );
    context.stop().await;
}

#[tokio::test]
async fn unchanged_pull_report_for_same_result_id_reuses_cached_items() {
    let context = make_client_with(
        json!({
            "capabilities": pull_capabilities(),
            "diagnosticResponses": [
                { "report": { "kind": "full", "resultId": "same-version", "items": [diagnostic_json("cached-full")] } },
                { "report": { "kind": "unchanged", "resultId": "same-version" } },
            ],
        }),
        options(500, 5.0),
    )
    .await;
    context
        .client
        .open_file(&context.source)
        .await
        .expect("open");

    let first = context
        .client
        .diagnostics(&context.source, None)
        .await
        .expect("first");
    let second = context
        .client
        .diagnostics(&context.source, None)
        .await
        .expect("second");

    assert_eq!(first.items, vec![diagnostic("cached-full")]);
    assert_eq!(second.items, vec![diagnostic("cached-full")]);
    context.stop().await;
}

#[tokio::test]
async fn pull_server_closing_instead_of_answering_is_not_reported_clean() {
    let context = make_client_with(
        json!({ "capabilities": pull_capabilities(), "diagnosticResponses": [{ "action": "close" }] }),
        options(500, 5.0),
    )
    .await;
    context
        .client
        .open_file(&context.source)
        .await
        .expect("open");

    let error = context
        .client
        .diagnostics(&context.source, None)
        .await
        .expect_err("transport failure");

    let message = error.to_string().to_lowercase();
    assert!(
        message.contains("exited") || message.contains("closed"),
        "{message}"
    );
    context.stop().await;
}

#[tokio::test]
async fn explicitly_unsupported_pull_falls_back_to_matching_push_publish() {
    let context = make_client_with(
        json!({
            "capabilities": pull_capabilities(),
            "publishDiagnostics": [{ "trigger": "didOpen", "version": 1, "diagnostics": [diagnostic_json("push-fallback")] }],
            "diagnosticResponses": [{ "error": { "code": -32601, "message": "Method not found" } }],
        }),
        options(500, 5.0),
    )
    .await;

    let result = context
        .client
        .diagnostics(&context.source, None)
        .await
        .expect("diagnostics");

    assert_eq!(result.items, vec![diagnostic("push-fallback")]);
    context.stop().await;
}

#[tokio::test]
async fn timed_out_pull_request_sends_cancel_for_its_request_id() {
    let context = make_client_with(
        json!({ "capabilities": pull_capabilities(), "diagnosticResponses": [{ "action": "hang" }] }),
        LspClientOptions {
            timeouts: LspClientTimeoutOptions { request_timeout_ms: Some(30), initialize_timeout_ms: Some(5_000) },
            ..options(50, 5.0)
        },
    )
    .await;

    let result = context
        .client
        .diagnostics(&context.source, None)
        .await
        .expect("diagnostics");
    let cancels = wait_for_event_count(
        &context.events,
        |event| is_event(event, "clientNotification", "$/cancelRequest"),
        1,
    )
    .await;
    let request = read_events(&context.events)
        .into_iter()
        .find(is_pull_request)
        .expect("pull request");

    assert_eq!(transient_kind(&result), Some("freshness_timeout"));
    assert_eq!(cancels[0]["params"], json!({ "id": request["id"] }));
    context.stop().await;
}

#[tokio::test]
async fn never_publishing_push_server_resolves_clean_after_full_freshness_window() {
    let context = make_client_with(json!({}), options(60, 5.0)).await;

    let started = Instant::now();
    let result = context
        .client
        .diagnostics(&context.source, None)
        .await
        .expect("diagnostics");
    let elapsed = started.elapsed().as_millis();

    assert_eq!(result.transient_error, None);
    assert_eq!(result.items, vec![]);
    assert!(elapsed >= 45, "{elapsed}");
    context.stop().await;
}

#[tokio::test]
async fn unsupported_later_pull_after_change_does_not_return_stale_cached_pull() {
    let context = make_client_with(
        json!({
            "capabilities": pull_capabilities(),
            "diagnosticResponses": [
                { "report": { "kind": "full", "resultId": "v1", "items": [diagnostic_json("stale-pull")] } },
                { "error": { "code": -32601, "message": "Method not found" } },
            ],
        }),
        options(500, 5.0),
    )
    .await;
    let first = context
        .client
        .diagnostics(&context.source, None)
        .await
        .expect("first");
    assert_eq!(first.items, vec![diagnostic("stale-pull")]);
    std::fs::write(&context.source, "const changed = 1;\n").expect("write");
    context
        .client
        .open_file(&context.source)
        .await
        .expect("reopen");

    let second = context
        .client
        .diagnostics(&context.source, None)
        .await
        .expect("second");

    assert_eq!(second.transient_error, None);
    assert_eq!(second.items, vec![]);
    context.stop().await;
}

#[tokio::test]
async fn unsupported_later_pull_on_unchanged_document_returns_current_cached_pull() {
    let context = make_client_with(
        json!({
            "capabilities": pull_capabilities(),
            "diagnosticResponses": [
                { "report": { "kind": "full", "resultId": "v1", "items": [diagnostic_json("cached-full")] } },
                { "error": { "code": -32601, "message": "Method not found" } },
            ],
        }),
        options(500, 5.0),
    )
    .await;
    let first = context
        .client
        .diagnostics(&context.source, None)
        .await
        .expect("first");
    assert_eq!(first.items, vec![diagnostic("cached-full")]);

    let second = context
        .client
        .diagnostics(&context.source, None)
        .await
        .expect("second");

    assert_eq!(second.transient_error, None);
    assert_eq!(second.items, vec![diagnostic("cached-full")]);
    context.stop().await;
}
