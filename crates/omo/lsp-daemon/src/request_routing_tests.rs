use super::*;
use crate::ipc_protocol::auth_envelope;
use crate::ownership::EndpointIdentity;
use pretty_assertions::assert_eq;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

fn temp_project() -> (tempfile::TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path())
        .unwrap()
        .to_string_lossy()
        .into_owned();
    (dir, root)
}

fn join(root: &str, name: &str) -> String {
    Path::new(root).join(name).to_string_lossy().into_owned()
}

fn valid_context(root: &str) -> Value {
    json!({
        "cwd": root,
        "projectConfigPaths": [join(root, "lsp.json")],
        "userConfigPath": join(root, "user-lsp.json"),
        "installDecisionsPath": join(root, "install-decisions.json"),
        "capabilities": {"installDecisionTool": true},
    })
}

fn authenticated_tool_call(id: u64, token: &str, args: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": {"_omo": auth_envelope(token), "name": "status", "arguments": args}})
}

fn owner() -> DaemonOwner {
    DaemonOwner {
        pid: 1,
        nonce: "n".to_string(),
        started_at: "t".to_string(),
        endpoint: EndpointIdentity::Missing {
            path: "/tmp/x.sock".to_string(),
        },
    }
}

/// Mock of lsp-core dispatch: counts calls and, when `gate` is set, waits on it.
fn counting_dispatch(calls: Arc<AtomicUsize>, gate: Option<Arc<tokio::sync::Notify>>) -> Dispatch {
    Arc::new(move |input: Value, _context, signal: Option<AbortSignal>| {
        let calls = Arc::clone(&calls);
        let gate = gate.clone();
        Box::pin(async move {
            calls.fetch_add(1, AtomicOrdering::SeqCst);
            if let Some(gate) = gate {
                tokio::select! {
                    () = gate.notified() => {}
                    () = async { match &signal { Some(signal) => signal.cancelled().await, None => std::future::pending().await } } => {}
                }
            }
            Some(
                json!({"jsonrpc": "2.0", "id": input.get("id").cloned().unwrap_or(Value::Null), "result": {"content": [{"type": "text", "text": "core dispatched"}]}}),
            )
        })
    })
}

fn state(token: &str, dispatch: Dispatch, active: Option<ActiveRequests>) -> DaemonRouteState {
    DaemonRouteState {
        token: token.to_string(),
        owner: owner(),
        active_requests: active,
        dispatch,
    }
}

fn response_code(response: &Value) -> Option<&str> {
    response.pointer("/error/data/code").and_then(Value::as_str)
}

#[test]
fn non_tools_call_has_no_context_and_input_unchanged() {
    let raw = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {}});
    let routed = extract_request_context(&raw).unwrap();
    assert_eq!(routed.context, None);
    assert_eq!(routed.input, raw);
}

#[test]
fn tools_call_context_is_parsed_and_stripped_from_arguments() {
    let (_dir, root) = temp_project();
    let raw = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": "diagnostics", "arguments": {"filePath": "/a.ts", "_context": valid_context(&root)}}});
    let routed = extract_request_context(&raw).unwrap();
    let context = routed.context.unwrap();
    assert_eq!(
        serde_json::to_value(&context).unwrap(),
        valid_context(&root)
    );
    assert_eq!(
        routed.input["params"]["arguments"],
        json!({"filePath": "/a.ts"})
    );
    assert_eq!(routed.input["params"]["name"], json!("diagnostics"));
}

#[test]
fn tools_call_without_context_is_rejected() {
    let raw = json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "status", "arguments": {}}});
    assert_eq!(
        extract_request_context(&raw).unwrap_err().0,
        "Daemon tools/call arguments must include _context."
    );
    let bad_params = json!({"method": "tools/call", "params": 1});
    assert_eq!(
        extract_request_context(&bad_params).unwrap_err().0,
        "Daemon tools/call params must be an object."
    );
    let bad_args = json!({"method": "tools/call", "params": {"arguments": []}});
    assert_eq!(
        extract_request_context(&bad_args).unwrap_err().0,
        "Daemon tools/call arguments must be an object."
    );
}

#[test]
fn tools_call_with_empty_or_non_object_context_is_rejected() {
    let empty = json!({"method": "tools/call", "params": {"name": "status", "arguments": {"_context": {}}}});
    assert!(extract_request_context(&empty).is_err());
    let scalar = json!({"method": "tools/call", "params": {"name": "status", "arguments": {"_context": "x"}}});
    assert_eq!(
        extract_request_context(&scalar).unwrap_err().0,
        "LSP request _context must be an object."
    );
}

#[tokio::test]
async fn omitted_context_returns_invalid_request_before_dispatch() {
    let calls = Arc::new(AtomicUsize::new(0));
    let state = state("secret", counting_dispatch(Arc::clone(&calls), None), None);
    let response = handle_daemon_message(&authenticated_tool_call(5, "secret", json!({})), &state)
        .await
        .unwrap();
    assert_eq!(response_code(&response), Some("invalid_daemon_request"));
    assert_eq!(response["error"]["code"], json!(-32602));
    assert_eq!(response["id"], json!(5));
    assert_eq!(calls.load(AtomicOrdering::SeqCst), 0);
}

#[tokio::test]
async fn authenticated_cancellation_aborts_and_removes_the_active_request() {
    let (_dir, root) = temp_project();
    let calls = Arc::new(AtomicUsize::new(0));
    let active: ActiveRequests = Arc::default();
    let state = state(
        "secret",
        counting_dispatch(
            Arc::clone(&calls),
            Some(Arc::new(tokio::sync::Notify::new())),
        ),
        Some(Arc::clone(&active)),
    );
    let request = authenticated_tool_call(7, "secret", json!({"_context": valid_context(&root)}));
    let running = tokio::spawn({
        let state = state.clone();
        async move { handle_daemon_message(&request, &state).await }
    });
    while !active.lock().unwrap().contains_key("7") {
        tokio::task::yield_now().await;
    }
    let signal = active.lock().unwrap()["7"].controller.signal();
    let cancel = json!({"jsonrpc": "2.0", "method": "$/cancelRequest", "params": {"_omo": auth_envelope("secret"), "id": 7}});
    assert_eq!(handle_daemon_message(&cancel, &state).await, None);
    assert!(signal.aborted());
    running.await.unwrap();
    assert!(active.lock().unwrap().is_empty());
}

#[tokio::test]
async fn unauthenticated_cancellation_does_not_abort() {
    let active: ActiveRequests = Arc::default();
    let controller = AbortController::new();
    active.lock().unwrap().insert(
        "9".to_string(),
        ActiveRequest {
            serial: 0,
            controller: controller.clone(),
        },
    );
    let state = state(
        "secret",
        counting_dispatch(Arc::default(), None),
        Some(Arc::clone(&active)),
    );
    let cancel = json!({"jsonrpc": "2.0", "method": "$/cancelRequest", "params": {"_omo": auth_envelope("wrong"), "id": 9}});
    let response = handle_daemon_message(&cancel, &state).await.unwrap();
    assert_eq!(
        response_code(&response),
        Some("daemon_authentication_failed")
    );
    assert!(!controller.signal().aborted());
    assert!(active.lock().unwrap().contains_key("9"));
}

#[cfg(unix)]
#[test]
fn symlinked_project_config_escaping_cwd_is_rejected() {
    let (_dir, root) = temp_project();
    let (_outside_dir, outside) = temp_project();
    std::fs::write(join(&outside, "lsp.json"), "{}").unwrap();
    std::os::unix::fs::symlink(join(&outside, "lsp.json"), join(&root, "lsp.json")).unwrap();
    let raw = json!({"method": "tools/call", "params": {"name": "status", "arguments": {"_context": valid_context(&root)}}});
    assert!(extract_request_context(&raw).is_err());
}

#[cfg(unix)]
#[test]
fn symlinked_project_config_inside_cwd_is_canonicalized() {
    let (_dir, root) = temp_project();
    std::fs::create_dir(join(&root, "real")).unwrap();
    std::fs::write(join(&root, "real/lsp.json"), "{}").unwrap();
    std::os::unix::fs::symlink(join(&root, "real/lsp.json"), join(&root, "lsp.json")).unwrap();
    let raw = json!({"method": "tools/call", "params": {"name": "status", "arguments": {"_context": valid_context(&root)}}});
    let context = extract_request_context(&raw).unwrap().context.unwrap();
    assert_eq!(
        context.project_config_paths,
        vec![join(&root, "real/lsp.json")]
    );
}

#[cfg(unix)]
#[test]
fn missing_config_below_symlinked_parent_canonicalizes_nearest_ancestor() {
    let (_dir, root) = temp_project();
    std::fs::create_dir(join(&root, "real")).unwrap();
    std::os::unix::fs::symlink(join(&root, "real"), join(&root, "link")).unwrap();
    let mut context = valid_context(&root);
    context["projectConfigPaths"] = json!([join(&root, "link/missing/lsp.json")]);
    let raw = json!({"method": "tools/call", "params": {"name": "status", "arguments": {"_context": context}}});
    let parsed = extract_request_context(&raw).unwrap().context.unwrap();
    assert_eq!(
        parsed.project_config_paths,
        vec![join(&root, "real/missing/lsp.json")]
    );
}

#[tokio::test]
async fn ping_reports_protocol_version_and_owner() {
    let state = state("secret", counting_dispatch(Arc::default(), None), None);
    let ping = json!({"jsonrpc": "2.0", "id": 1, "method": "omo/ping", "params": {"_omo": auth_envelope("secret")}});
    let response = handle_daemon_message(&ping, &state).await.unwrap();
    assert_eq!(
        response,
        json!({"jsonrpc": "2.0", "id": 1, "result": {"protocolVersion": 1, "pid": 1, "nonce": "n", "startedAt": "t", "endpoint": {"kind": "missing", "path": "/tmp/x.sock"}}})
    );
}
