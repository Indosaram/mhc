//! Ports of auth-ownership.test.ts and daemon-roundtrip.test.ts against a real daemon.

mod common;

use std::sync::{Arc, Mutex};

use common::{context_for, short_tempdir, test_paths, within};
use lsp_core::request_context::LspRequestContext;
use lsp_daemon::daemon_client::{CallToolOptions, call_tool_via_daemon};
use lsp_daemon::daemon_server::{DaemonServerHandle, DaemonServerOptions, start_daemon_server};
use lsp_daemon::ensure_daemon::{PROBE_TIMEOUT_MS, probe_daemon};
use lsp_daemon::ipc_protocol::{auth_envelope, read_auth_token};
use lsp_daemon::ownership::{
    DaemonOwner, EndpointIdentity, StartupError, endpoint_identity, read_daemon_owner,
    remove_daemon_metadata_for_owner, write_daemon_owner,
};
use lsp_daemon::paths::DaemonPaths;
use lsp_daemon::request_routing::Dispatch;
use lsp_daemon::socket_jsonrpc::{LineBuffer, encode_json_line};
use serde_json::{Map, Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

fn options() -> DaemonServerOptions {
    DaemonServerOptions {
        on_idle_shutdown: Some(Arc::new(|| {})),
        skip_version_reap: true,
        ..DaemonServerOptions::default()
    }
}

type Seen = Arc<Mutex<Vec<(Value, Option<LspRequestContext>)>>>;

/// Records what reaches Core and answers with a text block naming the context cwd.
fn recording_dispatch(seen: Seen) -> Dispatch {
    Arc::new(
        move |input: Value, context: Option<LspRequestContext>, _signal| {
            let seen = Arc::clone(&seen);
            Box::pin(async move {
                let cwd = context
                    .as_ref()
                    .map(|context| context.cwd.clone())
                    .unwrap_or_default();
                seen.lock().unwrap().push((input.clone(), context));
                Some(
                    json!({"jsonrpc": "2.0", "id": input["id"], "result": {"content": [{"type": "text", "text": format!("cwd={cwd}")}], "isError": false}}),
                )
            })
        },
    )
}

async fn start(paths: &DaemonPaths, dispatch: Option<Dispatch>) -> DaemonServerHandle {
    within(start_daemon_server(
        paths,
        DaemonServerOptions {
            dispatch,
            ..options()
        },
    ))
    .await
    .unwrap()
}

async fn exchange(paths: &DaemonPaths, message: &Value) -> Value {
    let mut stream = tokio::net::UnixStream::connect(&paths.socket)
        .await
        .unwrap();
    stream
        .write_all(encode_json_line(message).as_bytes())
        .await
        .unwrap();
    let mut decoder = LineBuffer::new();
    let mut chunk = [0_u8; 8192];
    within(async {
        loop {
            let read = stream.read(&mut chunk).await.unwrap();
            assert!(read > 0, "daemon closed without answering");
            if let Some(Ok(line)) = decoder.push(&chunk[..read]).into_iter().next() {
                return line;
            }
        }
    })
    .await
}

fn call_options(paths: &DaemonPaths, context: LspRequestContext) -> CallToolOptions {
    CallToolOptions {
        context: Some(context),
        paths: Some(paths.clone()),
        ensure: Some(common::noop_ensure()),
        ..CallToolOptions::default()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn auth_is_required_and_tokens_are_never_forwarded() {
    let root = short_tempdir();
    let paths = test_paths(&root);
    let seen: Seen = Arc::default();
    let server = start(&paths, Some(recording_dispatch(Arc::clone(&seen)))).await;

    let unauthenticated = exchange(
        &paths,
        &json!({"jsonrpc": "2.0", "id": 1, "method": "omo/ping"}),
    )
    .await;
    assert_eq!(
        unauthenticated["error"]["data"]["code"],
        json!("daemon_authentication_failed")
    );
    let wrong = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"_omo": auth_envelope("wrong"), "name": "status", "arguments": {}}});
    assert_eq!(
        exchange(&paths, &wrong).await["error"]["code"],
        json!(-32001)
    );
    assert!(seen.lock().unwrap().is_empty());

    let result = call_tool_via_daemon(
        "status",
        Map::new(),
        call_options(&paths, context_for(root.path())),
    )
    .await
    .unwrap();
    assert!(!result.is_error);
    let (input, _context) = seen.lock().unwrap()[0].clone();
    let token = read_auth_token(&paths).unwrap();
    assert!(!input.to_string().contains(&token));
    assert!(input["params"].get("_omo").is_none());
    assert!(input["params"]["arguments"].get("_context").is_none());
    server.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn wrong_protocol_is_rejected_before_dispatch() {
    let root = short_tempdir();
    let paths = test_paths(&root);
    let seen: Seen = Arc::default();
    let server = start(&paths, Some(recording_dispatch(Arc::clone(&seen)))).await;
    let token = read_auth_token(&paths).unwrap();
    let message = json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"_omo": {"protocolVersion": 2, "token": token}, "name": "status", "arguments": {}}});
    let response = exchange(&paths, &message).await;
    assert_eq!(
        response["error"]["data"]["code"],
        json!("daemon_protocol_mismatch")
    );
    assert!(seen.lock().unwrap().is_empty());
    server.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn two_contexts_through_one_daemon_keep_their_cwd_scopes() {
    let root = short_tempdir();
    let paths = test_paths(&root);
    let (first, second) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let server = start(&paths, Some(recording_dispatch(Arc::default()))).await;
    let (a, b) = tokio::join!(
        call_tool_via_daemon(
            "status",
            Map::new(),
            call_options(&paths, context_for(first.path()))
        ),
        call_tool_via_daemon(
            "status",
            Map::new(),
            call_options(&paths, context_for(second.path()))
        ),
    );
    let canonical = |dir: &tempfile::TempDir| {
        std::fs::canonicalize(dir.path())
            .unwrap()
            .to_string_lossy()
            .into_owned()
    };
    assert_eq!(a.unwrap().text(), format!("cwd={}", canonical(&first)));
    assert_eq!(b.unwrap().text(), format!("cwd={}", canonical(&second)));
    server.close().await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn state_paths_use_private_modes() {
    use std::os::unix::fs::PermissionsExt;
    let root = short_tempdir();
    let paths = test_paths(&root);
    let server = start(&paths, None).await;
    let mode =
        |path: &std::path::Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&paths.dir), 0o700);
    for file in [
        &paths.auth,
        &paths.owner,
        &paths.pid,
        &paths.endpoint,
        &paths.socket,
    ] {
        assert_eq!(mode(file), 0o600, "{}", file.display());
    }
    server.close().await;
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn symlinked_state_directory_fails_before_binding() {
    let root = short_tempdir();
    let target = root.path().join("elsewhere");
    std::fs::create_dir_all(&target).unwrap();
    let paths = test_paths(&root);
    std::os::unix::fs::symlink(&target, &paths.dir).unwrap();
    assert!(start_daemon_server(&paths, options()).await.is_err());
    assert!(!target.join("daemon.sock").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn regular_file_at_state_directory_fails_closed() {
    let root = short_tempdir();
    let paths = test_paths(&root);
    std::fs::write(&paths.dir, "not a dir").unwrap();
    assert!(start_daemon_server(&paths, options()).await.is_err());
    assert!(std::fs::metadata(&paths.dir).unwrap().is_file());
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn loose_current_user_state_directory_is_accepted_and_made_private() {
    use std::os::unix::fs::PermissionsExt;
    let root = short_tempdir();
    let paths = test_paths(&root);
    std::fs::create_dir_all(&paths.dir).unwrap();
    std::fs::set_permissions(&paths.dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    let server = start(&paths, None).await;
    assert_eq!(
        std::fs::metadata(&paths.dir).unwrap().permissions().mode() & 0o777,
        0o700
    );
    server.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn existing_authenticated_owner_makes_candidate_exit_cleanly() {
    let root = short_tempdir();
    let paths = test_paths(&root);
    let server = start(&paths, None).await;
    let owner = read_daemon_owner(&paths);
    let second = within(start_daemon_server(&paths, options())).await;
    assert!(matches!(second, Err(StartupError::AlreadyRunning)));
    assert_eq!(read_daemon_owner(&paths), owner);
    server.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn live_unreachable_owner_defers_without_unlinking() {
    let root = short_tempdir();
    let paths = test_paths(&root);
    common::write_token(&paths, &"x".repeat(43));
    let live = DaemonOwner {
        pid: std::process::id(),
        nonce: "live".to_string(),
        started_at: "now".to_string(),
        endpoint: EndpointIdentity::Missing {
            path: paths.socket.to_string_lossy().into_owned(),
        },
    };
    write_daemon_owner(&paths, &live).unwrap();
    let result = within(start_daemon_server(&paths, options())).await;
    assert!(
        matches!(result, Err(StartupError::Deferred(_))),
        "{:?}",
        result.err()
    );
    assert!(paths.owner.exists());
    assert!(
        !paths.lock.exists(),
        "a deferred candidate must release the startup lock"
    );
}

/// A crashed daemon leaves its lock, owner, pid and a dead socket file behind; the next
/// candidate must reclaim all of it without hanging or panicking.
#[tokio::test(flavor = "multi_thread")]
async fn crashed_owner_leftovers_are_reclaimed_and_auth_rotated() {
    let root = short_tempdir();
    let paths = test_paths(&root);
    common::write_token(&paths, "old-token");
    drop(std::os::unix::net::UnixListener::bind(&paths.socket).unwrap());
    assert!(
        paths.socket.exists(),
        "stale socket file from the crashed owner"
    );
    let dead = DaemonOwner {
        pid: 9_999_999,
        nonce: "dead".to_string(),
        started_at: "old".to_string(),
        endpoint: endpoint_identity(&paths.socket.to_string_lossy()),
    };
    write_daemon_owner(&paths, &dead).unwrap();
    std::fs::write(&paths.lock, "9999999").unwrap();

    let server = start(&paths, None).await;
    assert_ne!(read_auth_token(&paths).unwrap(), "old-token");
    assert_ne!(read_daemon_owner(&paths).unwrap().nonce, "dead");
    assert!(within(probe_daemon(&paths, PROBE_TIMEOUT_MS, None)).await);
    server.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn stale_close_from_old_owner_keeps_winner_metadata() {
    let root = short_tempdir();
    let paths = test_paths(&root);
    let first = start(&paths, None).await;
    let old_owner = read_daemon_owner(&paths).unwrap();
    first.close().await;
    let second = start(&paths, None).await;
    remove_daemon_metadata_for_owner(&paths, &old_owner);
    assert!(paths.owner.exists());
    assert!(within(probe_daemon(&paths, PROBE_TIMEOUT_MS, None)).await);
    second.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn status_tool_returns_content_over_the_socket() {
    let root = short_tempdir();
    let paths = test_paths(&root);
    let server = start(&paths, None).await;
    let result = within(call_tool_via_daemon(
        "status",
        Map::new(),
        call_options(&paths, context_for(root.path())),
    ))
    .await
    .unwrap();
    assert!(!result.is_error, "{}", result.text());
    assert!(
        result.text().contains("Configured LSP servers:"),
        "{}",
        result.text()
    );
    server.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn two_racing_calls_both_receive_responses() {
    let root = short_tempdir();
    let paths = test_paths(&root);
    let server = start(&paths, None).await;
    let context = context_for(root.path());
    let (a, b) = within(async {
        tokio::join!(
            call_tool_via_daemon("status", Map::new(), call_options(&paths, context.clone())),
            call_tool_via_daemon(
                "lsp_status",
                Map::new(),
                call_options(&paths, context.clone())
            ),
        )
    })
    .await;
    assert!(!a.unwrap().is_error);
    assert!(!b.unwrap().is_error);
    server.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn unreachable_daemon_returns_structured_error() {
    let root = short_tempdir();
    let paths = test_paths(&root);
    let result = within(call_tool_via_daemon(
        "status",
        Map::new(),
        call_options(&paths, context_for(root.path())),
    ))
    .await
    .unwrap();
    assert!(result.is_error);
    assert!(
        result.text().starts_with("LSP daemon unreachable: "),
        "{}",
        result.text()
    );
    assert!(
        result
            .text()
            .contains("never runs language servers in-process")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn project_config_in_request_context_is_honored_per_request() {
    let root = short_tempdir();
    let paths = test_paths(&root);
    let server = start(&paths, None).await;
    let default_context = context_for(root.path());
    let baseline = within(call_tool_via_daemon(
        "status",
        Map::new(),
        call_options(&paths, default_context.clone()),
    ))
    .await
    .unwrap();
    let builtin_id = baseline
        .text()
        .lines()
        .find_map(|line| {
            line.strip_prefix("- ")
                .and_then(|rest| rest.split(':').next())
        })
        .unwrap()
        .to_string();
    let project = tempfile::tempdir().unwrap();
    std::fs::write(
        project.path().join("lsp-client.json"),
        json!({"lsp": {builtin_id.clone(): {"disabled": true}}}).to_string(),
    )
    .unwrap();
    let scoped = within(call_tool_via_daemon(
        "status",
        Map::new(),
        call_options(&paths, context_for(project.path())),
    ))
    .await
    .unwrap();
    assert!(
        scoped.text().contains(&format!("- {builtin_id}: disabled")),
        "{}",
        scoped.text()
    );
    let unscoped = within(call_tool_via_daemon(
        "status",
        Map::new(),
        call_options(&paths, default_context),
    ))
    .await
    .unwrap();
    assert!(unscoped.text().contains(&format!("- {builtin_id}: ")));
    assert!(
        !unscoped
            .text()
            .contains(&format!("- {builtin_id}: disabled"))
    );
    server.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn closing_removes_socket_and_pid_files() {
    let root = short_tempdir();
    let paths = test_paths(&root);
    let server = start(&paths, None).await;
    assert!(paths.socket.exists() && paths.pid.exists());
    server.close().await;
    assert!(!paths.socket.exists());
    assert!(!paths.pid.exists());
    assert!(!paths.owner.exists());
    assert!(server.is_closed());
}

#[tokio::test(flavor = "multi_thread")]
async fn idle_daemon_shuts_itself_down() {
    let root = short_tempdir();
    let paths = test_paths(&root);
    let server = within(start_daemon_server(
        &paths,
        DaemonServerOptions {
            idle_shutdown_ms: Some(0),
            idle_check_interval_ms: Some(5),
            skip_version_reap: true,
            ..DaemonServerOptions::default()
        },
    ))
    .await
    .unwrap();
    within(server.closed()).await;
    assert!(!paths.socket.exists());
}
