//! Ports of daemon-client-retry.test.ts, daemon-client.test.ts and client-surface.test.ts.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use common::{
    FakeDaemon, Reply, context_for, short_tempdir, spawn_fake_daemon, test_paths, tool_result,
    within, write_token,
};
use lsp_core::abort::AbortController;
use lsp_daemon::daemon_client::{
    CallToolOptions, EnsureFn, call_tool_via_daemon, current_request_context,
};
use lsp_daemon::daemon_request_error::DaemonCallError;
use lsp_daemon::paths::DaemonPaths;
use lsp_daemon::runtime_contract::Env;
use serde_json::{Map, Value, json};

const TOKEN: &str = "retry-test-token";

fn options(paths: &DaemonPaths, root: &tempfile::TempDir, timeout_ms: u64) -> CallToolOptions {
    CallToolOptions {
        context: Some(context_for(root.path())),
        paths: Some(paths.clone()),
        request_timeout_ms: Some(timeout_ms),
        ensure: Some(common::noop_ensure()),
        signal: None,
    }
}

fn setup() -> (tempfile::TempDir, DaemonPaths) {
    let root = short_tempdir();
    let paths = test_paths(&root);
    write_token(&paths, TOKEN);
    (root, paths)
}

fn token_of(message: &Value) -> Option<&str> {
    message
        .pointer("/params/_omo/token")
        .and_then(Value::as_str)
}

fn cancel_of(fake: &FakeDaemon) -> Option<Value> {
    fake.received()
        .into_iter()
        .find(|message| message["method"] == json!("$/cancelRequest"))
}

#[tokio::test(flavor = "multi_thread")]
async fn written_request_that_times_out_is_not_retried() {
    let (root, paths) = setup();
    let fake = spawn_fake_daemon(&paths.socket, Arc::new(|_message, _index| Reply::Hold));
    let result = within(call_tool_via_daemon(
        "status",
        Map::new(),
        options(&paths, &root, 50),
    ))
    .await
    .unwrap();
    assert!(result.is_error);
    assert!(
        result.text().contains("daemon request timed out"),
        "{}",
        result.text()
    );
    assert!(!result.text().contains("unreachable"));
    assert_eq!(fake.tool_calls(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn timeout_sends_authenticated_cancel_for_the_proxy_id() {
    let (root, paths) = setup();
    let fake = spawn_fake_daemon(&paths.socket, Arc::new(|_message, _index| Reply::Hold));
    // Burn id 1 so the request id cannot collide with a client-chosen id.
    let _warmup = within(call_tool_via_daemon(
        "status",
        Map::new(),
        options(&paths, &root, 20),
    ))
    .await;
    let result = within(call_tool_via_daemon(
        "status",
        Map::new(),
        options(&paths, &root, 50),
    ))
    .await
    .unwrap();
    assert!(result.is_error);
    fake.wait_until(|fake| {
        fake.received()
            .iter()
            .filter(|m| m["method"] == json!("$/cancelRequest"))
            .count()
            >= 2
    })
    .await;
    let received = fake.received();
    let request = received
        .iter()
        .rev()
        .find(|m| m["method"] == json!("tools/call"))
        .unwrap();
    let cancel = received
        .iter()
        .rev()
        .find(|m| m["method"] == json!("$/cancelRequest"))
        .unwrap();
    assert_ne!(request["id"], json!(1));
    assert_eq!(cancel["params"]["id"], request["id"]);
    assert_eq!(token_of(cancel), Some(TOKEN));
}

#[tokio::test(flavor = "multi_thread")]
async fn caller_abort_after_write_sends_cancel_and_reports_cancelled() {
    let (root, paths) = setup();
    let fake = spawn_fake_daemon(&paths.socket, Arc::new(|_message, _index| Reply::Hold));
    let controller = AbortController::new();
    let mut call = options(&paths, &root, 5_000);
    call.signal = Some(controller.signal());
    let running = tokio::spawn(call_tool_via_daemon("status", Map::new(), call));
    fake.wait_until(|fake| fake.tool_calls() == 1).await;
    controller.abort();
    let result = within(running).await.unwrap().unwrap();
    assert!(result.is_error);
    assert!(result.text().contains("cancelled"));
    assert!(!result.text().contains("unreachable"));
    fake.wait_until(|fake| cancel_of(fake).is_some()).await;
    let cancel = cancel_of(&fake).unwrap();
    assert_eq!(cancel["params"]["id"], fake.received()[0]["id"]);
    assert_eq!(token_of(&cancel), Some(TOKEN));
}

#[tokio::test(flavor = "multi_thread")]
async fn socket_appearing_after_first_connect_failure_is_retried_once() {
    let (root, paths) = setup();
    let ensure_calls = Arc::new(AtomicUsize::new(0));
    let fake_slot: Arc<std::sync::Mutex<Option<FakeDaemon>>> = Arc::default();
    let ensure: EnsureFn = Arc::new({
        let (ensure_calls, fake_slot) = (Arc::clone(&ensure_calls), Arc::clone(&fake_slot));
        move |paths, _signal| {
            let call = ensure_calls.fetch_add(1, Ordering::SeqCst) + 1;
            let fake_slot = Arc::clone(&fake_slot);
            Box::pin(async move {
                if call == 2 {
                    let fake = spawn_fake_daemon(
                        &paths.socket,
                        Arc::new(|message, _index| Reply::Respond(tool_result(message, "ok"))),
                    );
                    *fake_slot.lock().unwrap() = Some(fake);
                }
                Ok(())
            })
        }
    });
    let mut call = options(&paths, &root, 2_000);
    call.ensure = Some(ensure);
    let result = within(call_tool_via_daemon("status", Map::new(), call))
        .await
        .unwrap();
    assert_eq!(result.text(), "ok");
    assert_eq!(fake_slot.lock().unwrap().as_ref().unwrap().tool_calls(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn close_after_write_is_not_retried() {
    let (root, paths) = setup();
    let fake = spawn_fake_daemon(&paths.socket, Arc::new(|_message, _index| Reply::Close));
    let result = within(call_tool_via_daemon(
        "status",
        Map::new(),
        options(&paths, &root, 2_000),
    ))
    .await
    .unwrap();
    assert!(result.is_error);
    assert!(
        result.text().contains("daemon connection closed"),
        "{}",
        result.text()
    );
    assert_eq!(fake.tool_calls(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn rename_that_cannot_connect_is_not_retried() {
    let (root, paths) = setup();
    let ensure_calls = Arc::new(AtomicUsize::new(0));
    let mut call = options(&paths, &root, 50);
    call.ensure = Some(Arc::new({
        let ensure_calls = Arc::clone(&ensure_calls);
        move |_paths, _signal| {
            ensure_calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Ok(()) })
        }
    }));
    let args = json!({"filePath": "a.ts", "line": 1, "character": 0, "newName": "b"});
    let result = within(call_tool_via_daemon(
        "rename",
        args.as_object().unwrap().clone(),
        call,
    ))
    .await
    .unwrap();
    assert!(result.is_error);
    assert!(result.text().contains("daemon"));
    assert_eq!(ensure_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn invalid_response_after_write_is_not_retried() {
    let (root, paths) = setup();
    let fake = spawn_fake_daemon(
        &paths.socket,
        Arc::new(|message, _index| {
            Reply::Respond(
                json!({"jsonrpc": "2.0", "id": message["id"], "result": {"unexpected": true}}),
            )
        }),
    );
    let result = within(call_tool_via_daemon(
        "status",
        Map::new(),
        options(&paths, &root, 2_000),
    ))
    .await
    .unwrap();
    assert!(
        result.text().contains("invalid daemon response"),
        "{}",
        result.text()
    );
    assert_eq!(fake.tool_calls(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn auth_rejection_refreshes_the_token_once_and_retries() {
    let (root, paths) = setup();
    let fresh = "fresh-token-after-restart";
    let auth_path = paths.auth.clone();
    let fake = spawn_fake_daemon(
        &paths.socket,
        Arc::new(move |message, index| {
            if index == 0 {
                // The daemon restarted with a new token before answering.
                std::fs::write(&auth_path, fresh).unwrap();
                return Reply::Respond(
                    json!({"jsonrpc": "2.0", "id": message["id"], "error": {"code": -32001, "message": "Unauthorized", "data": {"code": "daemon_authentication_failed"}}}),
                );
            }
            Reply::Respond(tool_result(message, "ok"))
        }),
    );
    let result = within(call_tool_via_daemon(
        "status",
        Map::new(),
        options(&paths, &root, 2_000),
    ))
    .await
    .unwrap();
    assert_eq!(result.text(), "ok");
    assert_eq!(fake.tool_calls(), 2);
    let tokens: Vec<String> = fake
        .received()
        .iter()
        .filter_map(|m| token_of(m).map(str::to_string))
        .collect();
    assert_eq!(tokens, vec![TOKEN.to_string(), fresh.to_string()]);
    assert_eq!(std::fs::read_to_string(&paths.auth).unwrap().trim(), fresh);
}

#[test]
fn current_request_context_builds_exact_codex_defaults() {
    let env: Env = [
        ("HOME", "/home/me"),
        ("LSP_TOOLS_MCP_PROJECT_CONFIG", "/ignored/lsp.json"),
        ("SECRET", "x"),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_string(), value.to_string()))
    .collect();
    let context = current_request_context(&env).unwrap();
    let cwd = std::fs::canonicalize(std::env::current_dir().unwrap()).unwrap();
    assert_eq!(std::path::PathBuf::from(&context.cwd), cwd);
    let project = std::path::Path::new(&context.project_config_paths[0]);
    assert!(project.ends_with(".codex/lsp-client.json"));
    assert_eq!(context.user_config_path, "/home/me/.codex/lsp-client.json");
    assert_eq!(
        context.install_decisions_path,
        "/home/me/.codex/lsp-install-decisions.json"
    );
    assert!(context.capabilities.install_decision_tool);
    assert!(!serde_json::to_string(&context).unwrap().contains("SECRET"));
}

#[test]
fn current_request_context_without_lsp_env_is_still_complete() {
    let env: Env = [("PATH".to_string(), "/usr/bin".to_string())]
        .into_iter()
        .collect();
    let context = current_request_context(&env).unwrap();
    assert_eq!(context.project_config_paths.len(), 1);
    assert!(context.project_config_paths[0].ends_with("lsp-client.json"));
    assert!(context.capabilities.install_decision_tool);
}

#[tokio::test(flavor = "multi_thread")]
async fn omitted_context_rejects_before_daemon_dispatch() {
    let (root, paths) = setup();
    let fake = spawn_fake_daemon(
        &paths.socket,
        Arc::new(|message, _index| Reply::Respond(tool_result(message, "ok"))),
    );
    let ensure_calls = Arc::new(AtomicUsize::new(0));
    let mut call = options(&paths, &root, 500);
    call.context = None;
    call.ensure = Some(Arc::new({
        let ensure_calls = Arc::clone(&ensure_calls);
        move |_paths, _signal| {
            ensure_calls.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Ok(()) })
        }
    }));
    let error = call_tool_via_daemon("status", Map::new(), call)
        .await
        .unwrap_err();
    assert!(
        matches!(&error, DaemonCallError::Request(request) if request.message == "daemon tool context is required")
    );
    assert_eq!(ensure_calls.load(Ordering::SeqCst), 0);
    assert_eq!(fake.tool_calls(), 0);
}
