use crate::support;
use maho_ext_mcp::{
    config_schema::{McpServerConfig, Transport},
    connection::ServerConnection,
    log::McpLogger,
    shared_connection::SharedMcpConnection,
};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
#[tokio::test(start_paused = true)]
async fn last_owner_idle_disposal_waits_for_in_flight_request_cancellation() {
    let root = tempfile::tempdir().unwrap();
    let physical = ServerConnection::new(
        "shared",
        McpServerConfig {
            enabled: Some(true),
            ..Default::default()
        },
        None,
        Arc::new(Mutex::new(
            McpLogger::new("shared", root.path(), None).unwrap(),
        )),
    );
    let shared = SharedMcpConnection::with_idle_timeout(
        physical.clone(),
        root.path().into(),
        "hash".into(),
        Duration::from_secs(5),
        Duration::from_secs(60),
    );
    let registry = maho_ext_mcp::host_registry::HostMcpRegistry::default();
    let lease = registry.attach_shared("key", 1, || shared.clone()).unwrap();
    let (started, ready) = tokio::sync::oneshot::channel();
    let active = shared.clone();
    let task = tokio::spawn(async move {
        active
            .run_request(async {
                started.send(()).unwrap();
                std::future::pending::<()>().await
            })
            .await;
    });
    ready.await.unwrap();
    lease.dispose();
    assert_eq!(shared.lease_count(), 0);
    assert_eq!(
        physical.state(),
        maho_ext_mcp::connection::ServerConnectionState::Idle
    );
    let mut changes = physical.on_state_change();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    let event = tokio::time::timeout(Duration::from_secs(61), changes.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        event.state,
        maho_ext_mcp::connection::ServerConnectionState::Disabled
    );
    let replacement = ServerConnection::new(
        "replacement",
        McpServerConfig {
            enabled: Some(true),
            ..Default::default()
        },
        None,
        Arc::new(Mutex::new(
            McpLogger::new("replacement", root.path(), None).unwrap(),
        )),
    );
    let replacement_shared = SharedMcpConnection::new(
        replacement.clone(),
        root.path().into(),
        "hash".into(),
        Duration::from_secs(5),
    );
    let replacement_lease = registry
        .attach_shared("key", 2, || replacement_shared.clone())
        .unwrap();
    assert!(!Arc::ptr_eq(&replacement_lease.shared, &shared));
    registry.dispose().await.unwrap();
    assert_eq!(
        replacement_lease.state(),
        maho_ext_mcp::connection::ServerConnectionState::Disabled
    );
    shared.dispose().await.unwrap();
}
#[tokio::test]
async fn renewing_one_lease_never_replaces_the_other_owners_process() {
    let root = tempfile::tempdir().unwrap();
    let config = McpServerConfig {
        enabled: Some(true),
        transport: Some(Transport::Stdio),
        command: Some(support::node()),
        args: Some(vec![support::senpi(
            "packages/coding-agent/test/mcp/fixtures/stdio-server.ts",
        )]),
        ..Default::default()
    };
    let physical = ServerConnection::new(
        "shared",
        config,
        None,
        Arc::new(Mutex::new(
            McpLogger::new("shared", root.path(), None).unwrap(),
        )),
    );
    let shared = SharedMcpConnection::new(
        physical.clone(),
        root.path().into(),
        "hash".into(),
        Duration::from_secs(5),
    );
    let first = shared.attach(1, "key".into()).unwrap();
    let second = shared.attach(2, "key".into()).unwrap();
    first.connect().await.unwrap();
    let pid = physical.get_root_pid();
    let generation = second.generation();
    first.renew().await.unwrap();
    assert_eq!(second.generation(), generation);
    assert_eq!(physical.get_root_pid(), pid);
    assert!(!first.catalog().await.unwrap().tools.is_empty());
    first.dispose();
    assert!(
        first
            .request("ping", serde_json::json!({}), Duration::from_secs(5))
            .await
            .is_err()
    );
    second
        .request("ping", serde_json::json!({}), Duration::from_secs(5))
        .await
        .unwrap();
    assert_eq!(shared.lease_count(), 1);
    second.dispose();
    assert_eq!(shared.lease_count(), 0);
    shared.dispose().await.unwrap();
}

#[tokio::test]
async fn released_lease_cannot_degrade_other_owner_and_invalidation_bumps_generation() {
    use maho_ext_mcp::{
        connection::ServerConnectionState,
        errors::{McpError, McpErrorKind},
    };
    let root = tempfile::tempdir().unwrap();
    let physical = ServerConnection::new(
        "shared",
        McpServerConfig {
            enabled: Some(true),
            ..Default::default()
        },
        None,
        Arc::new(Mutex::new(
            McpLogger::new("shared", root.path(), None).unwrap(),
        )),
    );
    let shared = SharedMcpConnection::new(
        physical.clone(),
        root.path().into(),
        "hash".into(),
        Duration::from_secs(5),
    );
    let first = shared.attach(1, "key".into()).unwrap();
    let second = shared.attach(2, "key".into()).unwrap();
    first.dispose();
    first.mark_failure(
        ServerConnectionState::Degraded,
        Some(McpError::new(McpErrorKind::Connect, "late failure")),
    );
    assert_eq!(second.state(), ServerConnectionState::Idle);
    assert!(second.last_error().is_none());
    let generation = second.generation();
    second.invalidate().await.unwrap();
    assert!(second.generation() > generation);
    assert!(first.invalidate().await.is_err());
    shared.dispose().await.unwrap();
}

#[tokio::test]
async fn a_changed_generation_rekeys_every_lease_until_its_owner_releases() {
    // shared-connection.ts #renewedConfig: one owner's renewal that finds a changed snapshot re-keys every lease.
    use std::sync::atomic::{AtomicUsize, Ordering};
    let root = tempfile::tempdir().unwrap();
    let physical = ServerConnection::new(
        "shared",
        McpServerConfig {
            enabled: Some(true),
            ..Default::default()
        },
        None,
        Arc::new(Mutex::new(
            McpLogger::new("shared", root.path(), None).unwrap(),
        )),
    );
    let shared = SharedMcpConnection::new(
        physical,
        root.path().into(),
        "hash".into(),
        Duration::from_secs(5),
    );
    let first = shared.attach(1, "key".into()).unwrap();
    let second = shared.attach(2, "key".into()).unwrap();
    let rekeyed = [Arc::new(AtomicUsize::new(0)), Arc::new(AtomicUsize::new(0))];
    for (owner, count) in [(1, rekeyed[0].clone()), (2, rekeyed[1].clone())] {
        shared.on_generation_changed(
            owner,
            Arc::new(move || {
                count.fetch_add(1, Ordering::SeqCst);
            }),
        );
    }
    shared.notify_generation_changed();
    assert_eq!(
        rekeyed
            .iter()
            .map(|count| count.load(Ordering::SeqCst))
            .collect::<Vec<_>>(),
        [1, 1]
    );
    // A released owner no longer re-keys.
    first.dispose();
    shared.notify_generation_changed();
    assert_eq!(
        rekeyed
            .iter()
            .map(|count| count.load(Ordering::SeqCst))
            .collect::<Vec<_>>(),
        [1, 2]
    );
    second.dispose();
    shared.dispose().await.unwrap();
}

struct NullUi;
impl maho_ext_api::ExtensionUi for NullUi {
    fn select<'a>(
        &'a self,
        _: &'a str,
        _: &'a [String],
        _: maho_ext_api::ExtensionUiDialogOptions,
    ) -> maho_ext_api::UiFuture<'a, Option<String>> {
        Box::pin(async { None })
    }
    fn confirm<'a>(
        &'a self,
        _: &'a str,
        _: &'a str,
        _: maho_ext_api::ExtensionUiDialogOptions,
    ) -> maho_ext_api::UiFuture<'a, bool> {
        Box::pin(async { false })
    }
    fn input<'a>(
        &'a self,
        _: &'a str,
        _: Option<&'a str>,
        _: maho_ext_api::ExtensionUiDialogOptions,
    ) -> maho_ext_api::UiFuture<'a, Option<String>> {
        Box::pin(async { None })
    }
    fn notify(&self, _: &str, _: maho_ext_api::NotificationType) {}
    fn set_status(&self, _: &str, _: Option<&str>) {}
    fn set_widget(
        &self,
        _: &str,
        _: Option<maho_ext_api::WidgetContent>,
        _: maho_ext_api::ExtensionWidgetOptions,
    ) {
    }
    fn set_header(&self, _: Option<maho_ext_api::ComponentFactory>) {}
    fn set_footer(&self, _: Option<maho_ext_api::ComponentFactory>) {}
    fn set_title(&self, _: &str) {}
    fn paste_to_editor(&self, _: &str) {}
    fn set_editor_text(&self, _: &str) {}
    fn get_editor_text(&self) -> String {
        String::new()
    }
    fn custom(
        &self,
        _: maho_ext_api::ComponentFactory,
        _: maho_ext_api::CustomUiOptions,
    ) -> maho_ext_api::ExtensionFuture<'_, maho_ext_api::JsonValue> {
        Box::pin(async { Err(maho_ext_api::ExtensionFailure::new("no custom UI")) })
    }
    fn theme(&self) -> maho_ext_api::Theme {
        maho_ext_api::Theme::default()
    }
}

type ReadyListeners = Arc<Mutex<Vec<Arc<dyn Fn() + Send + Sync>>>>;

/// A UI bound to a private form presenter whose readiness the test flips; `subscribe` records listeners.
struct PrivateForms {
    ui: Arc<dyn maho_ext_api::ExtensionUi>,
    ready: Arc<std::sync::atomic::AtomicBool>,
    listeners: ReadyListeners,
    _abort: maho_ai::utils::abort::AbortController,
}
impl PrivateForms {
    fn new(ready: bool) -> Self {
        use maho_ext_mcp::form_types::{
            McpFormBinding, McpFormPresentation, bind_private_mcp_forms,
        };
        let ui: Arc<dyn maho_ext_api::ExtensionUi> = Arc::new(NullUi);
        let ready = Arc::new(std::sync::atomic::AtomicBool::new(ready));
        let listeners: ReadyListeners = Arc::default();
        let abort = maho_ai::utils::abort::AbortController::new();
        let signal = abort.signal();
        let capture = {
            let ready = ready.clone();
            let signal = signal.clone();
            Arc::new(move || {
                let ready = ready.clone();
                Some(McpFormPresentation {
                    signal: signal.clone(),
                    ready: Arc::new(move || ready.load(std::sync::atomic::Ordering::SeqCst)),
                    present: Arc::new(
                        |_: maho_ext_mcp::form_types::McpFormInput| -> futures::future::BoxFuture<
                            'static,
                            maho_ext_mcp::form_types::McpFormOutcome,
                        > { panic!("no form is presented") },
                    ),
                    synchronize: None,
                })
            })
        };
        let subscribe = {
            let listeners = listeners.clone();
            Arc::new(move |listener: Arc<dyn Fn() + Send + Sync>| {
                listeners.lock().unwrap().push(listener);
                let listeners = listeners.clone();
                Box::new(move || listeners.lock().unwrap().clear())
                    as maho_ext_mcp::form_types::McpFormUnsubscribe
            })
        };
        bind_private_mcp_forms(
            ui.clone(),
            McpFormBinding {
                epoch: "e".into(),
                signal,
                capture,
                dispose: Arc::new(|| {}),
                version: None,
                subscribe: Some(subscribe),
            },
        );
        Self {
            ui,
            ready,
            listeners,
            _abort: abort,
        }
    }
    fn fire(&self) {
        let listeners = self.listeners.lock().unwrap().clone();
        for listener in listeners {
            listener();
        }
    }
}

fn pooled(root: &std::path::Path) -> Arc<SharedMcpConnection> {
    let physical = ServerConnection::new(
        "shared",
        McpServerConfig {
            enabled: Some(true),
            ..Default::default()
        },
        None,
        Arc::new(Mutex::new(McpLogger::new("shared", root, None).unwrap())),
    );
    SharedMcpConnection::new(physical, root.into(), "hash".into(), Duration::from_secs(5))
}

#[tokio::test]
async fn readiness_aggregates_enforced_and_eligible_over_live_leases() {
    use maho_ext_mcp::shared_connection::SharedLeaseOptions;
    use std::sync::atomic::{AtomicUsize, Ordering};
    let root = tempfile::tempdir().unwrap();
    let shared = pooled(root.path());
    // No lease: nothing is eligible.
    assert!(!shared.readiness_enforced());
    assert!(!shared.readiness_eligible());
    let notified = Arc::new(AtomicUsize::new(0));
    let unsubscribe = shared.subscribe_readiness({
        let notified = notified.clone();
        Arc::new(move || {
            notified.fetch_add(1, Ordering::SeqCst);
        })
    });
    // Unenforced pool: every lease must be eligible.
    let plain = shared.attach(1, "key".into()).unwrap();
    assert_eq!(notified.load(Ordering::SeqCst), 1);
    shared.set_owner_options(
        1,
        SharedLeaseOptions {
            elicitation_ui: Some(Arc::new(NullUi)),
            ..Default::default()
        },
    );
    assert!(!shared.readiness_enforced());
    assert!(shared.readiness_eligible());
    let disabled = shared.attach(2, "key".into()).unwrap();
    shared.set_owner_options(
        2,
        SharedLeaseOptions {
            elicitation_enabled: false,
            ..Default::default()
        },
    );
    assert!(!shared.readiness_eligible());
    disabled.dispose();
    assert!(shared.readiness_eligible());
    // An enforced lease makes the pool enforced; any ready owner permits negotiation.
    let forms = PrivateForms::new(false);
    let private = shared.attach(3, "key".into()).unwrap();
    shared.set_owner_options(
        3,
        SharedLeaseOptions {
            elicitation_ui: Some(forms.ui.clone()),
            ..Default::default()
        },
    );
    assert!(shared.readiness_enforced());
    // The plain lease is eligible, so the enforced pool is eligible even though the presenter is not ready.
    assert!(shared.readiness_eligible());
    plain.dispose();
    assert!(!shared.readiness_eligible());
    forms.ready.store(true, Ordering::SeqCst);
    assert!(shared.readiness_eligible());
    // The lease's presenter readiness fans out to the connection's observers.
    let before = notified.load(Ordering::SeqCst);
    forms.fire();
    assert_eq!(notified.load(Ordering::SeqCst), before + 1);
    // Releasing the last lease unsubscribes its presenter observer.
    private.dispose();
    assert!(forms.listeners.lock().unwrap().is_empty());
    assert!(!shared.readiness_enforced());
    unsubscribe();
    let after = notified.load(Ordering::SeqCst);
    let again = shared.attach(4, "key".into()).unwrap();
    assert_eq!(notified.load(Ordering::SeqCst), after);
    again.dispose();
    shared.dispose().await.unwrap();
}

#[tokio::test]
async fn project_policy_resolves_from_an_active_lease_after_the_creator_releases() {
    use maho_ext_mcp::{
        shared_connection::SharedLeaseOptions,
        transport::{ProjectMcpMaterialization, ProjectPolicyResolver},
    };
    let root = tempfile::tempdir().unwrap();
    let shared = pooled(root.path());
    let policy = |tag: &'static str| -> ProjectPolicyResolver {
        Arc::new(move || -> futures::future::BoxFuture<'static, _> {
            Box::pin(async move {
                Ok(ProjectMcpMaterialization {
                    config: McpServerConfig::default(),
                    env: [("OWNER".to_owned(), tag.to_owned())].into(),
                })
            })
        })
    };
    let resolve = || async {
        let resolver = shared
            .connection
            .project_policy()
            .expect("aggregate resolver");
        resolver()
            .await
            .map(|materialized| materialized.env["OWNER"].clone())
    };
    let creator = shared.attach(1, "key".into()).unwrap();
    shared.set_owner_options(
        1,
        SharedLeaseOptions {
            project_policy: Some(policy("creator")),
            ..Default::default()
        },
    );
    let second = shared.attach(2, "key".into()).unwrap();
    shared.set_owner_options(
        2,
        SharedLeaseOptions {
            project_policy: Some(policy("second")),
            ..Default::default()
        },
    );
    assert_eq!(resolve().await.unwrap(), "creator");
    // An owner with a call in flight is preferred over attach order.
    let (entered, started) = tokio::sync::oneshot::channel();
    let (finish, finished) = tokio::sync::oneshot::channel::<()>();
    let call = tokio::spawn({
        let shared = shared.clone();
        async move {
            shared
                .call_tool(2, async move {
                    entered.send(()).unwrap();
                    finished.await.unwrap();
                })
                .await;
        }
    });
    started.await.unwrap();
    assert_eq!(resolve().await.unwrap(), "second");
    finish.send(()).unwrap();
    call.await.unwrap();
    // The creator's presentation closed while another lease remains.
    creator.dispose();
    assert_eq!(resolve().await.unwrap(), "second");
    // With no policy-bearing lease left, the creating owner's policy is the fallback.
    second.dispose();
    assert_eq!(resolve().await.unwrap(), "creator");
    shared.dispose().await.unwrap();
}
