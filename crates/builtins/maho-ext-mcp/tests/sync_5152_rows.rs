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
    // The renewal bumps the generation once, then its connect to the refused loopback port fails, so the
    // operation is not replayed: exactly one attempt and one renewal.
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
    assert_eq!(
        connection.generation(),
        1,
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

/// Row 42 (resources.ts `readMcpResourceAsText`): an origin-bound read refreshes credentials before any
/// request; a refresh needing auth marks the connection, and the failure text is redacted.
#[tokio::test]
async fn origin_bound_resource_read_refreshes_first_and_redacts_the_failure() {
    let secret = crate::reference_support::resolve_env("VALUE", "synthetic3858resourcealpha");
    let root = tempfile::tempdir().expect("root");
    let connection = ServerConnection::new(
        "resource",
        http_config(),
        None,
        logger("resource", root.path()),
    );
    let client = http_client("resource", root.path()).await;
    let refreshed = Arc::new(AtomicUsize::new(0));
    let observed = refreshed.clone();
    let issued = origin(
        connection.clone(),
        AbortSignal::default(),
        Arc::new(AtomicBool::new(true)),
    );
    let server = maho_ext_mcp::resources::McpResourceServer {
        server: "resource".into(),
        client,
        agent_dir: root.path().into(),
        artifacts: Arc::new(maho_ext_mcp::guard::output_guard::McpOutputArtifacts::default()),
        output_guard: None,
        request_timeout: Duration::from_secs(2),
        resources: vec![],
        connection: Some(connection.clone()),
        request_origin: Some(Arc::new(move || Some(issued.clone()))),
        ensure_fresh: Some(Arc::new(move || {
            observed.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Err(McpError::new(McpErrorKind::Auth, "refresh rejected")) })
        })),
    };
    let uri = format!("fixture://{secret}");
    let error = maho_ext_mcp::resources::read_mcp_resource_as_text(&server, &uri)
        .await
        .expect_err("refresh needs auth");
    assert_eq!(refreshed.load(Ordering::SeqCst), 1);
    assert_eq!(connection.state(), ServerConnectionState::NeedsAuth);
    assert!(!error.message.contains(&secret), "{}", error.message);
    assert!(
        error.message.starts_with(
            "Failed to read MCP resource fixture://<redacted>: MCP server resource needs OAuth"
        ),
        "{}",
        error.message
    );
    connection.dispose().await.expect("dispose");
}

/// An `ExtensionUi` registered with an enforced private-form binding whose presenter is ready.
fn enforced_private_form_ui() -> (
    Arc<dyn maho_ext_api::ExtensionUi>,
    maho_ai::utils::abort::AbortController,
) {
    use maho_ai::utils::abort::AbortController;
    use maho_core::mcp_form_types::{
        McpFormBinding, McpFormOutcome, McpFormOutcomeKind, McpFormOutcomeSource,
        McpFormPresentation,
    };
    use maho_core::mcp_mrtr_presentations::register_mcp_mrtr_presentation;

    let host = AbortController::new();
    let presentation = McpFormPresentation {
        signal: host.signal(),
        ready: Arc::new(|| true),
        present: Arc::new(|_| {
            futures::FutureExt::boxed(async {
                McpFormOutcome {
                    kind: McpFormOutcomeKind::ProcessingFailure,
                    source: McpFormOutcomeSource::HostTermination,
                }
            })
        }),
        synchronize: None,
    };
    let binding = McpFormBinding {
        epoch: "epoch".into(),
        signal: host.signal(),
        capture: Arc::new(move || Some(presentation.clone())),
        dispose: Arc::new(|| {}),
        version: None,
        subscribe: None,
    };
    let ui: Arc<dyn maho_ext_api::ExtensionUi> =
        Arc::new(crate::command_notify_redaction::RecordingUi::default());
    register_mcp_mrtr_presentation(ui.clone(), host.signal(), None, Some(binding));
    (ui, host)
}

fn write_http_server(agent_dir: &Path, url: &str) {
    std::fs::write(
        agent_dir.join("mcp.json"),
        json!({"mcpServers": {"fx": {
            "type": "http",
            "url": url,
            "auth": false,
            "exposure": "direct",
            "lifecycle": "lazy",
            "connectTimeoutMs": 5000,
        }}})
        .to_string(),
    )
    .expect("global mcp.json");
}

/// A service's pooled connection for `fx` (the physical connection every lease on that pool shares).
async fn pooled_connection(service: &maho_ext_mcp::service::McpService) -> Arc<ServerConnection> {
    let connection = &service.connections["fx"];
    assert!(connection.shared.is_some(), "an HTTP server is pooled");
    connection.entry.lock().await.connection.clone()
}

/// Row 08 (host-registry.ts `attachShared` through service-connection.ts): two sessions on one host registry
/// that declare the same server but differ in legacy-form policy land in different pools, while two sessions
/// with the same policy share one physical connection.
#[tokio::test]
async fn sessions_with_different_legacy_form_policies_never_share_a_pool() {
    use maho_ext_mcp::{host_registry::HostMcpRegistry, service::McpService};
    use std::collections::BTreeMap;

    let stub = crate::reference_support::HeaderStub::start().await;
    let agent = tempfile::tempdir().expect("agent dir");
    write_http_server(agent.path(), &stub.url);
    let registry = Arc::new(HostMcpRegistry::default());
    let (enforced_ui, _host) = enforced_private_form_ui();
    let plain_ui = || -> Arc<dyn maho_ext_api::ExtensionUi> {
        Arc::new(crate::command_notify_redaction::RecordingUi::default())
    };
    let mut services = Vec::new();
    for (owner, ui) in [
        (5152_08_1, plain_ui()),
        (5152_08_2, plain_ui()),
        (5152_08_3, enforced_ui),
    ] {
        let mut service = McpService::new(registry.clone(), owner);
        service.set_elicitation_ui(Some(ui));
        service
            .attach_session(agent.path(), agent.path(), &BTreeMap::new(), true, &[])
            .await
            .expect("attach");
        services.push(service);
    }
    let plain = pooled_connection(&services[0]).await;
    let peer = pooled_connection(&services[1]).await;
    let private = pooled_connection(&services[2]).await;
    assert!(
        Arc::ptr_eq(&plain, &peer),
        "the same legacy-form policy shares one pooled connection"
    );
    assert!(
        !Arc::ptr_eq(&plain, &private),
        "an enforced private-form session never joins a plain session's pool"
    );
    for mut service in services {
        service.dispose().await.expect("dispose");
    }
    registry.dispose().await.expect("registry dispose");
    stub.close().await;
}

/// Row 09 (service.ts `#requestOrigin` `isCurrent`): an origin issued for a declared server stops being current
/// when that server's credential identity changes or when the binding's config no longer declares it, so the
/// request is refused as a context change. The binding lifetime and the connection generation never change here.
#[tokio::test]
async fn request_origins_end_when_credentials_change_or_the_server_is_undeclared() {
    use maho_ext_mcp::{
        auth::token_store::{McpStoredAuth, McpTokenStore},
        host_registry::HostMcpRegistry,
        service::McpService,
    };
    use std::collections::BTreeMap;

    let stub = crate::reference_support::HeaderStub::start().await;
    let agent = tempfile::tempdir().expect("agent dir");
    let oauth_url = "http://127.0.0.1:9/mcp";
    let tokens = McpTokenStore::new(agent.path(), "oa", oauth_url);
    let token = |access: &str| McpStoredAuth {
        access_token: Some(access.into()),
        ..Default::default()
    };
    tokens.write(token("synthetic-5152-first")).expect("tokens");
    std::fs::write(
        agent.path().join("mcp.json"),
        json!({"mcpServers": {
            "fx": {"type": "http", "url": stub.url, "auth": false, "exposure": "direct", "lifecycle": "lazy",
                "connectTimeoutMs": 5000},
            "oa": {"type": "http", "url": oauth_url, "auth": "oauth", "exposure": "direct", "lifecycle": "lazy",
                "connectTimeoutMs": 5000},
        }})
        .to_string(),
    )
    .expect("global mcp.json");
    // A cached lazy server runs no startup connect, so nothing in the background can touch `oa` while the
    // test rotates its credential.
    seed_cached_catalog(agent.path(), "oa");
    let registry = Arc::new(HostMcpRegistry::default());
    let mut service = McpService::new(registry.clone(), 5152_09);
    service
        .attach_session(agent.path(), agent.path(), &BTreeMap::new(), true, &[])
        .await
        .expect("attach");

    // Credential identity: rotating the stored grant makes the origin stale; restoring it makes it current.
    let oauth = service.connections["oa"]
        .entry
        .lock()
        .await
        .connection
        .clone();
    let origin = service.request_origin("oa", &oauth);
    assert!(origin.current(), "issued under the admitted credential");
    tokens
        .write(token("synthetic-5152-second"))
        .expect("rotated tokens");
    assert!(!origin.current(), "a changed credential identity is stale");
    tokens
        .write(token("synthetic-5152-first"))
        .expect("restored tokens");
    assert!(
        origin.current(),
        "the check compares identities, not writes"
    );

    // Declaration: once the binding's config drops the server, its origin is refused.
    service.connect_server("fx").await.expect("connect");
    let plain = service.connections["fx"]
        .entry
        .lock()
        .await
        .connection
        .clone();
    let client = plain.client().expect("connected client");
    let origin = service.request_origin("fx", &plain);
    let generation = plain.generation();
    assert!(
        take_mcp_request_origin(&mcp_request_options(Some(origin.clone()), None), &client)
            .expect("current origin")
            .is_some()
    );
    service
        .config
        .as_mut()
        .expect("config")
        .servers
        .remove("fx");
    service.resync_current_config().await.expect("resync");
    assert!(!service.connections.contains_key("fx"));
    assert_eq!(
        plain.generation(),
        generation,
        "the pool kept its generation"
    );
    assert!(!origin.current(), "an undeclared server's origin is stale");
    assert_eq!(
        take_mcp_request_origin(&mcp_request_options(Some(origin), None), &client)
            .err()
            .map(|error| error.code),
        Some(McpMrtrErrorCode::ContextChanged)
    );
    service.dispose().await.expect("dispose");
    registry.dispose().await.expect("registry dispose");
    stub.close().await;
}

/// Row 12 (senpi 3796-mcp-project-env.test.ts `reconnects a project skill server with its edited env`): the
/// connection identity keeps the project declaration's hash, so an env-only edit of a project skill server
/// re-keys it and the next connect runs the new declaration.
#[tokio::test]
async fn project_skill_server_rekeys_when_only_its_env_changes() {
    use maho_ext_mcp::{
        host_registry::HostMcpRegistry,
        service::McpService,
        skills::{SkillLike, parse_skill_mcp_declarations},
    };
    use std::collections::BTreeMap;

    let root = tempfile::tempdir().expect("project root");
    let agent = tempfile::tempdir().expect("agent dir");
    let receipt = root.path().join("edit-receipt.json");
    let fixture = crate::support::senpi("packages/coding-agent/test/mcp/fixtures/stdio-server.ts");
    let script = format!(
        "import {{writeFileSync}} from \"node:fs\";writeFileSync({},JSON.stringify({{ref:process.env.REF}}));await import({});",
        json!(receipt),
        json!(fixture),
    );
    let declare = |reference: &str| {
        std::fs::write(
            root.path().join("mcp.json"),
            json!({"probe": {
                "command": crate::support::node(),
                "args": ["-e", script],
                "env": {"REF": reference},
            }})
            .to_string(),
        )
        .expect("skill sidecar");
        parse_skill_mcp_declarations(&[SkillLike {
            name: "carrier".into(),
            file_path: root.path().join("SKILL.md"),
            base_dir: root.path().into(),
            scope: Some("project".into()),
            ..Default::default()
        }])
    };
    let read_receipt = || -> serde_json::Value {
        serde_json::from_str(&std::fs::read_to_string(&receipt).expect("receipt")).expect("json")
    };
    let registry = Arc::new(HostMcpRegistry::default());
    let mut service = McpService::new(registry.clone(), 3796);
    service
        .attach_session(root.path(), agent.path(), &BTreeMap::new(), true, &[])
        .await
        .expect("attach");

    let warnings = service
        .attach_skill_mcp_servers(&declare("lit1"))
        .await
        .expect("first declaration");
    assert!(warnings.is_empty(), "{warnings:?}");
    let first = service.connections["probe"]
        .entry
        .lock()
        .await
        .connection
        .clone();
    service
        .connect_server("probe")
        .await
        .expect("first connect");
    assert_eq!(read_receipt(), json!({"ref": "lit1"}));

    // A different length keeps the skills.rs sidecar stamp (mtime, ctime, len, ino) from matching the first
    // write within one coarse timestamp tick.
    let warnings = service
        .attach_skill_mcp_servers(&declare("lit22"))
        .await
        .expect("edited declaration");
    assert!(warnings.is_empty(), "{warnings:?}");
    service.reconnect_server("probe").await.expect("reconnect");
    let second = service.connections["probe"]
        .entry
        .lock()
        .await
        .connection
        .clone();
    assert!(
        !Arc::ptr_eq(&first, &second),
        "the env edit re-keyed the server"
    );
    service
        .connect_server("probe")
        .await
        .expect("second connect");
    assert_eq!(read_receipt(), json!({"ref": "lit22"}));
    service.dispose().await.expect("dispose");
    registry.dispose().await.expect("registry dispose");
}

/// Seeds a valid catalog for `name` under its current config hash and credential identity, so a lazy server is
/// cached and runs no startup connect (service.ts: `shouldRaceMcpStartup(lifecycle) || cachedCatalog === undefined`).
fn seed_cached_catalog(agent_dir: &Path, name: &str) {
    use std::collections::BTreeMap;
    let config =
        maho_ext_mcp::config::load_mcp_config(maho_ext_mcp::config_schema::LoadMcpConfigOptions {
            cwd: agent_dir,
            agent_dir,
            env: &BTreeMap::new(),
            project_trusted: true,
            home_dir: Some(agent_dir),
            claude_config_dir: Some(agent_dir),
        })
        .expect("config");
    let server = &config.servers[name];
    let declared = server.config.as_ref().expect("declared config");
    let credential_identity = maho_ext_mcp::auth::catalog_identity::mcp_credential_identity(
        declared,
        name,
        Some(agent_dir),
        Some(&BTreeMap::new()),
    );
    write_mcp_cached_server(
        agent_dir,
        name,
        McpCachedServerCatalog {
            modern: false,
            config_hash: server.config_hash.clone().expect("config hash"),
            credential_identity,
            fetched_at: chrono::Utc::now().timestamp_millis() as f64,
            tools: vec![json!({"name": "tool_1", "inputSchema": {"type": "object"}})],
            resources: vec![],
            prompts: vec![],
            instructions: Some("cached instructions".into()),
        },
    )
    .expect("seed catalog");
}

/// Writes one OAuth server `oa` to `agent_dir`'s mcp.json with its stored grant, and seeds its catalog.
fn oauth_server_fixture(
    agent_dir: &Path,
    access: &str,
) -> maho_ext_mcp::auth::token_store::McpTokenStore {
    let url = "http://127.0.0.1:9/mcp";
    let tokens = maho_ext_mcp::auth::token_store::McpTokenStore::new(agent_dir, "oa", url);
    write_grant(&tokens, access);
    std::fs::write(
        agent_dir.join("mcp.json"),
        json!({"mcpServers": {"oa": {"type": "http", "url": url, "auth": "oauth", "exposure": "direct",
            "lifecycle": "lazy", "connectTimeoutMs": 5000}}})
        .to_string(),
    )
    .expect("global mcp.json");
    seed_cached_catalog(agent_dir, "oa");
    tokens
}

fn write_grant(tokens: &maho_ext_mcp::auth::token_store::McpTokenStore, access: &str) {
    tokens
        .write(maho_ext_mcp::auth::token_store::McpStoredAuth {
            access_token: Some(access.into()),
            ..Default::default()
        })
        .expect("tokens");
}

/// Row 09 (service.ts:782 `#syncFromConfig`, :669 `getConnection`, `#serverSnapshot`): a connection whose
/// credentials went stale is hidden from `get_connection`, the snapshots and the wire status, and the next sync
/// re-creates it instead of keeping it under its unchanged config key.
#[tokio::test]
async fn a_stale_credential_connection_is_hidden_then_recreated_by_the_next_sync() {
    use maho_ext_mcp::{
        host_registry::HostMcpRegistry,
        service::McpService,
        service_types::{McpLifecycleState, McpUnspawnedState},
    };
    use std::collections::BTreeMap;

    let agent = tempfile::tempdir().expect("agent dir");
    let tokens = oauth_server_fixture(agent.path(), "synthetic-5152-09-first");
    let registry = Arc::new(HostMcpRegistry::default());
    let mut service = McpService::new(registry.clone(), 5152_09_2);
    service
        .attach_session(agent.path(), agent.path(), &BTreeMap::new(), true, &[])
        .await
        .expect("attach");
    let admitted = service.connections["oa"]
        .entry
        .lock()
        .await
        .connection
        .clone();
    assert!(service.get_connection("oa").is_some());
    assert_eq!(
        oa_snapshot(&service).await.lifecycle_state,
        McpLifecycleState::Other(McpUnspawnedState::Cached)
    );
    assert_eq!(oa_wire(&service).await.tools.len(), 1);
    assert!(
        maho_ext_mcp::instructions::refresh_mcp_instructions_for_session(&service)
            .await
            .contains("cached instructions")
    );

    write_grant(&tokens, "synthetic-5152-09-second");
    assert!(
        service.get_connection("oa").is_none(),
        "a stale-credential connection is hidden"
    );
    let snapshot = oa_snapshot(&service).await;
    assert_eq!(
        snapshot.lifecycle_state,
        McpLifecycleState::Other(McpUnspawnedState::NotSpawned)
    );
    assert_eq!(snapshot.generation, None);
    let wire = oa_wire(&service).await;
    assert!(wire.tools.is_empty(), "the stale catalog is hidden");
    assert_eq!(
        wire.status,
        Some(maho_ext_mcp::service_types::McpWireServerStatus::Config(
            maho_ext_mcp::config_schema::McpServerState::Enabled
        ))
    );
    assert!(
        !maho_ext_mcp::instructions::refresh_mcp_instructions_for_session(&service)
            .await
            .contains("cached instructions"),
        "stale cached instructions are hidden"
    );

    // The next sync re-creates the connection under the new credential instead of keeping the stale one.
    seed_cached_catalog(agent.path(), "oa");
    service.resync_current_config().await.expect("resync");
    let recreated = service
        .get_connection("oa")
        .expect("re-created connection")
        .entry
        .lock()
        .await
        .connection
        .clone();
    assert!(
        !Arc::ptr_eq(&admitted, &recreated),
        "the stale connection was not kept"
    );
    service.dispose().await.expect("dispose");
    registry.dispose().await.expect("registry dispose");
}

/// Row 07 (service.ts `#serverSnapshot`): a `needs_auth` connection whose credential identity is gone reports
/// `needs_auth` with its own redacted error in the snapshots and the wire status, though the stale connection
/// itself is hidden.
#[tokio::test]
async fn needs_auth_without_a_credential_identity_reports_needs_auth() {
    use maho_ext_mcp::{
        host_registry::HostMcpRegistry,
        service::McpService,
        service_types::{McpLifecycleState, McpWireServerStatus},
    };
    use std::collections::BTreeMap;

    let agent = tempfile::tempdir().expect("agent dir");
    let tokens = oauth_server_fixture(agent.path(), "synthetic-5152-07-first");
    let registry = Arc::new(HostMcpRegistry::default());
    let mut service = McpService::new(registry.clone(), 5152_07);
    service
        .attach_session(agent.path(), agent.path(), &BTreeMap::new(), true, &[])
        .await
        .expect("attach");
    let connection = service.connections["oa"]
        .entry
        .lock()
        .await
        .connection
        .clone();
    connection.mark_failure(
        ServerConnectionState::NeedsAuth,
        Some(McpError::new(McpErrorKind::Auth, "sign in to oa")),
    );
    tokens.clear().expect("logout");
    assert!(
        service.get_connection("oa").is_none(),
        "the credential went stale"
    );

    let snapshot = oa_snapshot(&service).await;
    assert_eq!(
        snapshot.lifecycle_state,
        McpLifecycleState::Connection(ServerConnectionState::NeedsAuth)
    );
    assert_eq!(snapshot.last_error.as_deref(), Some("sign in to oa"));
    let wire = oa_wire(&service).await;
    assert_eq!(
        wire.status,
        Some(McpWireServerStatus::Connection(
            ServerConnectionState::NeedsAuth
        ))
    );
    assert_eq!(wire.last_error.as_deref(), Some("sign in to oa"));
    assert!(wire.tools.is_empty());
    service.dispose().await.expect("dispose");
    registry.dispose().await.expect("registry dispose");
}

/// `oa`'s rows by name: the loaded config may also hold servers from the user's home configs.
async fn oa_snapshot(
    service: &maho_ext_mcp::service::McpService,
) -> maho_ext_mcp::service_types::McpServerSnapshot {
    service
        .server_snapshots()
        .await
        .into_iter()
        .find(|snapshot| snapshot.name == "oa")
        .expect("oa snapshot")
}

async fn oa_wire(
    service: &maho_ext_mcp::service::McpService,
) -> maho_ext_mcp::service_types::McpWireStatusServer {
    service
        .wire_status_snapshot()
        .await
        .servers
        .into_iter()
        .find(|server| server.name == "oa")
        .expect("oa wire status")
}
