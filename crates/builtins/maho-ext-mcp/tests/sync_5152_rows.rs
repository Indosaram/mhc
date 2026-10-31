//! senpi-mcp rows whose ports had no behavioural test: catalog `modern` round trip (19), request lifetimes
//! (10, 39), request origins and legacy operations (38), no MRTR retry (43), and the wrap clock and close
//! observers (44). Each test fails against the pre-port code it names.

use maho_ext_api::AbortSignal;
use maho_ext_mcp::{
    catalog_cache::{McpCachedServerCatalog, read_mcp_catalog_cache, write_mcp_cached_server},
    config_schema::{Auth, McpServerConfig, Transport},
    connection::{ServerConnection, ServerConnectionState},
    errors::{McpError, McpErrorKind},
    health::{with_mcp_retriable_failed_send_retry, with_mcp_session_expiry_retry},
    log::McpLogger,
    modern::errors::{McpMrtrError, McpMrtrErrorCode, as_mcp_mrtr_error},
    request_origin::{
        LegacyMcpOperationLookup, McpRequestOrigin, begin_legacy_mcp_operation,
        create_mcp_request_origin, legacy_mcp_operation, mcp_request_options,
        take_mcp_request_origin,
    },
    shared_connection::SharedMcpConnection,
    transport_sdk::{McpClient, McpTransportSpec},
    wrap::{MCP_ELAPSED_CLOCK, McpClock, McpInterrupted, mcp_abortable_delay, on_mcp_client_close},
};
use serde_json::json;
use std::{
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

fn logger(name: &str, root: &Path) -> Arc<Mutex<McpLogger>> {
    Arc::new(Mutex::new(
        McpLogger::new(name, root, None).expect("logger"),
    ))
}

fn http_config() -> McpServerConfig {
    McpServerConfig {
        enabled: Some(true),
        transport: Some(Transport::Http),
        url: Some("http://127.0.0.1:1/mcp".into()),
        auth: Some(Auth::Disabled(false)),
        ..Default::default()
    }
}

/// An HTTP client is built without any network I/O.
async fn http_client(name: &str, root: &Path) -> Arc<McpClient> {
    McpClient::materialize(
        name,
        &McpTransportSpec::Http {
            url: "http://127.0.0.1:1/mcp".parse().expect("url"),
            headers: Default::default(),
        },
        logger(name, root),
    )
    .await
    .expect("http client")
}

fn origin(
    connection: Arc<ServerConnection>,
    signal: AbortSignal,
    current: Arc<AtomicBool>,
) -> Arc<McpRequestOrigin> {
    create_mcp_request_origin(McpRequestOrigin::new(
        1,
        connection,
        signal,
        None,
        None,
        false,
        "fixture".into(),
        None,
        Arc::new(move || current.load(Ordering::SeqCst)),
        Arc::new(|_: AbortSignal| {
            futures::FutureExt::boxed(async { Ok::<_, McpError>(json!({})) })
        }),
    ))
}

/// Row 19 (catalog-cache.ts): a catalog collected from a modern server persists its `modern` flag, and with
/// it the modern-only fields (`outputSchema`, boolean input schemas); a legacy entry drops them.
#[test]
fn catalog_cache_round_trips_the_modern_flag_and_modern_only_fields() {
    let root = tempfile::tempdir().expect("agent dir");
    let tool = json!({
        "name": "typed",
        "inputSchema": {"type": "object"},
        "outputSchema": {"type": "object", "properties": {"n": {"type": "number"}}},
    });
    let catalog = |modern| McpCachedServerCatalog {
        modern,
        config_hash: "synthetic-hmac".into(),
        credential_identity: None,
        fetched_at: 1.0,
        tools: vec![tool.clone()],
        resources: vec![],
        prompts: vec![],
        instructions: None,
    };
    write_mcp_cached_server(root.path(), "modern", catalog(true)).expect("write modern");
    write_mcp_cached_server(root.path(), "legacy", catalog(false)).expect("write legacy");
    let raw: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(root.path().join("cache/mcp-cache.json")).expect("cache file"),
    )
    .expect("cache json");
    assert_eq!(raw["servers"]["modern"]["modern"], true);
    assert!(raw["servers"]["legacy"].get("modern").is_none());

    let cache = read_mcp_catalog_cache(root.path());
    assert!(cache.servers["modern"].modern);
    assert_eq!(
        cache.servers["modern"].tools[0]["outputSchema"],
        tool["outputSchema"]
    );
    assert!(!cache.servers["legacy"].modern);
    assert!(
        cache.servers["legacy"].tools[0]
            .get("outputSchema")
            .is_none()
    );
}

/// Row 39 (connection.ts `requestLifetimeSignal`): invalidating a connection ends every request issued under
/// the previous lifetime and opens a fresh one; disabling ends it for good.
#[tokio::test]
async fn connection_request_lifetime_ends_on_invalidation_and_on_disable() {
    let root = tempfile::tempdir().expect("root");
    let connection = ServerConnection::new(
        "lifetime",
        http_config(),
        None,
        logger("lifetime", root.path()),
    );
    let first = connection.request_lifetime_signal();
    assert!(!first.is_aborted());
    connection.bump_generation().await.expect("bump");
    assert!(first.is_aborted(), "the previous generation's requests end");
    let second = connection.request_lifetime_signal();
    assert!(!second.is_aborted(), "a fresh lifetime replaces it");
    connection.disable().await.expect("disable");
    assert!(second.is_aborted());
    assert!(
        connection.request_lifetime_signal().is_aborted(),
        "a disabled connection issues no live lifetime"
    );
    connection.dispose().await.expect("dispose");
}

/// Row 10 (shared-lease.ts): each lease owns its request lifetime; one owner's bump or release never ends
/// another owner's requests.
#[tokio::test]
async fn shared_lease_request_lifetimes_are_per_owner() {
    let root = tempfile::tempdir().expect("root");
    let connection =
        ServerConnection::new("shared", http_config(), None, logger("shared", root.path()));
    let shared = SharedMcpConnection::new(
        connection,
        root.path().into(),
        "hash".into(),
        Duration::from_secs(1),
    );
    let left = shared.attach(1, "key".into()).expect("left lease");
    let right = shared.attach(2, "key".into()).expect("right lease");
    let (left_signal, right_signal) = (
        left.request_lifetime_signal(),
        right.request_lifetime_signal(),
    );
    left.bump_generation().expect("bump");
    assert!(left_signal.is_aborted());
    assert!(
        !right_signal.is_aborted(),
        "a peer's bump never ends this owner's requests"
    );
    assert!(!left.request_lifetime_signal().is_aborted());
    let renewed = left.request_lifetime_signal();
    left.dispose();
    assert!(renewed.is_aborted(), "release ends the owner's lifetime");
    assert!(!right_signal.is_aborted());
    right.dispose();
    assert!(right_signal.is_aborted());
    shared.dispose().await.expect("dispose");
}

/// Row 38 (request-origin.ts): an origin is taken only while current and only against the physical client
/// it was issued for; a legacy operation is found only when exactly one live one exists.
#[tokio::test]
async fn request_origins_and_legacy_operations_follow_upstream_ownership() {
    let root = tempfile::tempdir().expect("root");
    let connection =
        ServerConnection::new("origin", http_config(), None, logger("origin", root.path()));
    let client = http_client("origin", root.path()).await;
    let current = Arc::new(AtomicBool::new(true));

    // Not connected: the origin's connection has no client, so the request context changed.
    let issued = origin(connection.clone(), AbortSignal::default(), current.clone());
    let taken = take_mcp_request_origin(&mcp_request_options(Some(issued.clone()), None), &client);
    assert_eq!(
        taken.err().map(|error| error.code),
        Some(McpMrtrErrorCode::ContextChanged)
    );
    // No origin: nothing to validate.
    assert!(matches!(
        take_mcp_request_origin(&mcp_request_options(None, None), &client),
        Ok(None)
    ));

    assert!(matches!(
        legacy_mcp_operation(&client),
        LegacyMcpOperationLookup::None
    ));
    let first = begin_legacy_mcp_operation(&client, issued.clone(), None);
    match legacy_mcp_operation(&client) {
        LegacyMcpOperationLookup::One { origin, signal } => {
            assert!(Arc::ptr_eq(&origin, &issued));
            assert!(!signal.is_aborted());
        }
        _ => panic!("exactly one live operation"),
    }
    // A stale origin never owns a form.
    current.store(false, Ordering::SeqCst);
    assert!(matches!(
        legacy_mcp_operation(&client),
        LegacyMcpOperationLookup::None
    ));
    current.store(true, Ordering::SeqCst);
    // Two concurrent operations are ambiguous: neither may claim the server's form.
    let second = begin_legacy_mcp_operation(&client, issued.clone(), None);
    assert!(matches!(
        legacy_mcp_operation(&client),
        LegacyMcpOperationLookup::Ambiguous
    ));
    drop(second);
    assert!(matches!(
        legacy_mcp_operation(&client),
        LegacyMcpOperationLookup::One { .. }
    ));
    // The operation's signal follows the origin's lifetime.
    let lifetime = AbortSignal::default();
    drop(first);
    let bound = begin_legacy_mcp_operation(
        &client,
        origin(connection.clone(), lifetime.clone(), current.clone()),
        None,
    );
    let LegacyMcpOperationLookup::One { signal, .. } = legacy_mcp_operation(&client) else {
        panic!("the bound operation is live");
    };
    lifetime.abort();
    tokio::time::timeout(Duration::from_secs(5), signal.cancelled())
        .await
        .expect("the operation signal follows the origin lifetime");
    assert!(matches!(
        legacy_mcp_operation(&client),
        LegacyMcpOperationLookup::None
    ));
    drop(bound);
    connection.dispose().await.expect("dispose");
}

/// Row 43 (health.ts): an MRTR failure is final; neither the session-expiry nor the failed-send retry replays
/// it, even when its text looks like a session expiry or a retriable transport failure.
#[tokio::test]
async fn mrtr_errors_are_never_retried() {
    let root = tempfile::tempdir().expect("root");
    let connection =
        ServerConnection::new("mrtr", http_config(), None, logger("mrtr", root.path()));
    // Each MRTR failure carries text that would otherwise trigger that retry.
    let mrtr = |message: &'static str| {
        let mut error: McpError = McpMrtrError::new(McpMrtrErrorCode::TransportFailed).into();
        error.message = message.into();
        error.retriable = true;
        error
    };
    let attempts = AtomicUsize::new(0);
    let result = with_mcp_session_expiry_retry(&connection, || {
        attempts.fetch_add(1, Ordering::SeqCst);
        let error = mrtr("MCP error -32001: session expired");
        async move { Err::<(), _>(error) }
    })
    .await;
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
    assert!(as_mcp_mrtr_error(&result.expect_err("final")).is_some());

    let attempts = AtomicUsize::new(0);
    let result = with_mcp_retriable_failed_send_retry(&connection, || {
        attempts.fetch_add(1, Ordering::SeqCst);
        let error = mrtr("connection refused");
        async move { Err::<(), _>(error) }
    })
    .await;
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
    assert!(as_mcp_mrtr_error(&result.expect_err("final")).is_some());
    assert_eq!(
        connection.state(),
        ServerConnectionState::Idle,
        "no reconnect was attempted"
    );
    assert_eq!(connection.generation(), 0);

    // Control: the same text without the MRTR marker is retried (so the guard is what stops it).
    let attempts = AtomicUsize::new(0);
    let _ = with_mcp_session_expiry_retry(&connection, || {
        attempts.fetch_add(1, Ordering::SeqCst);
        async {
            Err::<(), _>(McpError::new(
                McpErrorKind::SessionExpired,
                "session expired",
            ))
        }
    })
    .await;
    assert!(attempts.load(Ordering::SeqCst) >= 1);
    assert_ne!(
        connection.generation(),
        0,
        "a plain expiry renews the connection"
    );
    connection.dispose().await.expect("dispose");
}

/// Row 44 (wrap.ts `mcpElapsedClock` / `mcpAbortableDelay`): the clock is monotonic tokio time, and a delay
/// resolves on time or rejects as soon as its signal aborts.
#[tokio::test(start_paused = true)]
async fn elapsed_clock_and_abortable_delay_follow_tokio_time() {
    // The clock base is process-wide and may postdate this paused runtime's start; move past it first.
    tokio::time::advance(Duration::from_secs(3600)).await;
    let before = MCP_ELAPSED_CLOCK.now();
    tokio::time::advance(Duration::from_millis(250)).await;
    let elapsed = MCP_ELAPSED_CLOCK.now() - before;
    assert!((elapsed - 250.0).abs() < 1e-6, "{elapsed}");

    let signal = AbortSignal::default();
    assert_eq!(
        mcp_abortable_delay(&MCP_ELAPSED_CLOCK, 1000.0, &signal).await,
        Ok(())
    );
    let after = MCP_ELAPSED_CLOCK.now() - before;
    // Paused time auto-advances to the timer's millisecond tick, never before the deadline.
    assert!((1250.0..1252.0).contains(&after), "{after}");

    let aborted = AbortSignal::default();
    aborted.abort();
    assert_eq!(
        mcp_abortable_delay(&MCP_ELAPSED_CLOCK, 1000.0, &aborted).await,
        Err(McpInterrupted)
    );
    assert_eq!(
        MCP_ELAPSED_CLOCK.now() - before,
        after,
        "an aborted delay never waits"
    );

    let late = AbortSignal::default();
    let delay = mcp_abortable_delay(&MCP_ELAPSED_CLOCK, 60_000.0, &late);
    tokio::pin!(delay);
    assert!(futures::poll!(&mut delay).is_pending());
    late.abort();
    assert_eq!(delay.await, Err(McpInterrupted));
}

/// Row 44 (wrap.ts `onMcpClientClose`): every observer of a client close fires, and a dropped observer is
/// unsubscribed without affecting the others.
#[tokio::test]
async fn client_close_observers_fire_independently() {
    let root = tempfile::tempdir().expect("root");
    let client = http_client("close", root.path()).await;
    let (first_tx, first_rx) = tokio::sync::oneshot::channel();
    let (second_tx, second_rx) = tokio::sync::oneshot::channel();
    let dropped_fired = Arc::new(AtomicBool::new(false));
    let first = on_mcp_client_close(&client, move || {
        let _ = first_tx.send(());
    });
    let second = on_mcp_client_close(&client, move || {
        let _ = second_tx.send(());
    });
    let flag = dropped_fired.clone();
    let dropped = on_mcp_client_close(&client, move || flag.store(true, Ordering::SeqCst));
    drop(dropped);
    client.closed.send_replace(true);
    tokio::time::timeout(Duration::from_secs(5), first_rx)
        .await
        .expect("bounded")
        .expect("first observer");
    tokio::time::timeout(Duration::from_secs(5), second_rx)
        .await
        .expect("bounded")
        .expect("second observer");
    assert!(!dropped_fired.load(Ordering::SeqCst));
    // An observer registered after close fires immediately.
    let (late_tx, late_rx) = tokio::sync::oneshot::channel();
    let late = on_mcp_client_close(&client, move || {
        let _ = late_tx.send(());
    });
    tokio::time::timeout(Duration::from_secs(5), late_rx)
        .await
        .expect("bounded")
        .expect("late observer");
    drop((first, second, late));
}

/// Row 08 (host-registry.ts `attachShared` via service-connection.ts): pooled connections are keyed by
/// legacy-form policy. A session without private forms keys by eligibility; an enforced private-form session
/// keys as `private:` regardless of the presenter's current readiness, so readiness changes never fork a pool.
#[test]
fn shared_pools_key_by_legacy_form_policy() {
    use maho_ai::utils::abort::AbortController;
    use maho_core::mcp_form_types::{
        McpFormBinding, McpFormOutcome, McpFormOutcomeKind, McpFormOutcomeSource,
        McpFormPresentation,
    };
    use maho_core::mcp_mrtr_presentations::register_mcp_mrtr_presentation;
    use maho_ext_mcp::service_connection::mcp_legacy_form_policy_key;

    let plain: Arc<dyn maho_ext_api::ExtensionUi> =
        Arc::new(crate::command_notify_redaction::RecordingUi::default());
    assert_eq!(mcp_legacy_form_policy_key(None, true), "true:false");
    assert_eq!(mcp_legacy_form_policy_key(Some(&plain), true), "true:false");
    assert_eq!(
        mcp_legacy_form_policy_key(Some(&plain), false),
        "false:false"
    );

    let ready = Arc::new(AtomicBool::new(false));
    let host = AbortController::new();
    let presentation = {
        let ready = ready.clone();
        let signal = host.signal();
        McpFormPresentation {
            signal,
            ready: Arc::new(move || ready.load(Ordering::SeqCst)),
            present: Arc::new(|_| {
                futures::FutureExt::boxed(async {
                    McpFormOutcome {
                        kind: McpFormOutcomeKind::ProcessingFailure,
                        source: McpFormOutcomeSource::HostTermination,
                    }
                })
            }),
            synchronize: None,
        }
    };
    let binding = McpFormBinding {
        epoch: "epoch".into(),
        signal: host.signal(),
        capture: Arc::new(move || Some(presentation.clone())),
        dispose: Arc::new(|| {}),
        version: None,
        subscribe: None,
    };
    let enforced: Arc<dyn maho_ext_api::ExtensionUi> =
        Arc::new(crate::command_notify_redaction::RecordingUi::default());
    register_mcp_mrtr_presentation(enforced.clone(), host.signal(), None, Some(binding));
    assert_eq!(
        mcp_legacy_form_policy_key(Some(&enforced), true),
        "private:true"
    );
    ready.store(true, Ordering::SeqCst);
    assert_eq!(
        mcp_legacy_form_policy_key(Some(&enforced), true),
        "private:true",
        "readiness never re-keys an enforced pool"
    );
    // An enforced binding advertises legacy forms only while its presenter is ready.
    assert!(maho_ext_mcp::elicitation::legacy_mcp_form_eligible(
        Some(&enforced),
        true
    ));
    ready.store(false, Ordering::SeqCst);
    assert!(!maho_ext_mcp::elicitation::legacy_mcp_form_eligible(
        Some(&enforced),
        true
    ));
    assert!(maho_ext_mcp::elicitation::legacy_mcp_form_eligible(
        Some(&plain),
        true
    ));
}
