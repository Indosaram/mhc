//! Resolved lifecycle dependencies (`lifecycle/context.ts` + `LifecycleDeps` in `port.ts`).

use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use utils::process_sweep::{create_default_process_killer, default_is_process_alive};

use isolation_core::OwnerProbe;

use super::port::{
    LifecycleReattachPorts, OrphanSignal, ProcessSignaller, ReattachPort, ResidencyRegistry,
    RespawnPort,
};
use super::host_session::{
    HostEndpointPort, HostSessionProbe, HostSessionRetryPolicy, NoHostEndpoint,
    default_host_session_retry_policy,
};
use super::host_session_close::HostSessionCloser;
use super::host_session_default::{
    HostTransport, NoHostTransport, default_host_session_closer, default_host_session_probe,
};
use super::residency::BatchAdmissionOptions;
use super::settings::TaskSettings;
use super::store_port::LifecycleStore;
use crate::isolation::IsolationRuntime;
use crate::state::TaskStatus;

const DEFAULT_ORPHAN_KILL_DELAY_MS: u64 = 5_000;
const DEFAULT_HOST_CLOSE_TIMEOUT_MS: u64 = 10_000;

pub type NowFn = Arc<dyn Fn() -> i64 + Send + Sync>;
pub type DequeuePendingFn = Arc<dyn Fn(&str) + Send + Sync>;

/// Caller-supplied lifecycle dependencies; unset optionals take production defaults.
pub struct LifecycleDeps {
    pub store: Arc<dyn LifecycleStore>,
    pub registry: Arc<dyn ResidencyRegistry>,
    pub config: TaskSettings,
    pub now: Option<NowFn>,
    pub signaller: Option<Arc<dyn ProcessSignaller>>,
    pub respawn: Option<RespawnPort>,
    pub reattach: Option<ReattachPort>,
    pub orphan_kill_delay_ms: Option<u64>,
    pub host_pid: Option<i64>,
    pub dequeue_pending: Option<DequeuePendingFn>,
    pub reconcile_admission: Option<BatchAdmissionOptions>,
    /// The isolation runtime the reconcile salvage/sweep drives; `None` skips both.
    pub isolation: Option<Arc<dyn IsolationRuntime>>,
    /// The ownership probe the sweep uses; defaults to the producer's process probe.
    pub isolation_probe: Option<Arc<dyn OwnerProbe>>,
    /// The host-owned daemon transport; `None` uses [`NoHostTransport`] (nothing reachable/closeable).
    pub host_transport: Option<Arc<dyn HostTransport>>,
    /// Liveness of daemon-hosted children; defaults to the production probe over the transport.
    pub host_session_probe: Option<Arc<dyn HostSessionProbe>>,
    /// The ONLY way a session this process does not hold is ended; defaults to the production closer.
    pub host_session_close: Option<HostSessionCloser>,
    pub host_retry: Option<HostSessionRetryPolicy>,
    pub host_endpoint: Option<Arc<dyn HostEndpointPort>>,
    pub host_close_timeout_ms: Option<u64>,
}

impl LifecycleDeps {
    pub fn new(
        store: Arc<dyn LifecycleStore>,
        registry: Arc<dyn ResidencyRegistry>,
        config: TaskSettings,
    ) -> Self {
        Self {
            store,
            registry,
            config,
            now: None,
            signaller: None,
            respawn: None,
            reattach: None,
            orphan_kill_delay_ms: None,
            host_pid: None,
            dequeue_pending: None,
            reconcile_admission: None,
            isolation: None,
            isolation_probe: None,
            host_transport: None,
            host_session_probe: None,
            host_session_close: None,
            host_retry: None,
            host_endpoint: None,
            host_close_timeout_ms: None,
        }
    }
}

#[derive(Clone)]
pub struct LifecycleContext {
    pub store: Arc<dyn LifecycleStore>,
    pub registry: Arc<dyn ResidencyRegistry>,
    pub config: TaskSettings,
    pub now: NowFn,
    pub signaller: Arc<dyn ProcessSignaller>,
    pub orphan_kill_delay_ms: u64,
    pub host_pid: i64,
    pub dequeue_pending: DequeuePendingFn,
    pub reattach_ports: Option<LifecycleReattachPorts>,
    pub reconcile_admission: BatchAdmissionOptions,
    /// The isolation runtime the reconcile salvage/sweep drives; `None` skips both.
    pub isolation: Option<Arc<dyn IsolationRuntime>>,
    /// The ownership probe the sweep uses (TS isolationProbe ?? processOwnerProbe).
    pub isolation_probe: Arc<dyn OwnerProbe>,
    /// Liveness of daemon-hosted children: ONE probeHost + ONE list_sessions per pass, matched by
    /// session path. `host_session_close` is the ONLY way a session this process does not hold is ended.
    pub host_session_probe: Arc<dyn HostSessionProbe>,
    pub host_session_close: Option<HostSessionCloser>,
    pub host_retry: HostSessionRetryPolicy,
    pub host_endpoint: Arc<dyn HostEndpointPort>,
    pub host_close_timeout_ms: u64,
}

/// Probes with `kill(pid, 0)` and signals through the utils process killer.
pub struct DefaultSignaller;

impl ProcessSignaller for DefaultSignaller {
    fn is_alive(&self, pid: i64) -> bool {
        u32::try_from(pid).is_ok_and(default_is_process_alive)
    }

    fn signal(&self, pid: i64, signal: OrphanSignal) {
        let Ok(pid32) = u32::try_from(pid) else {
            return;
        };
        let killer = create_default_process_killer(std::env::consts::OS);
        let result = match signal {
            OrphanSignal::Term => killer.terminate(pid32),
            OrphanSignal::Kill => killer.kill(pid32),
        };
        if let Err(error) = result {
            utils::logger::log(
                "senpi-task orphan signal skipped",
                Some(&json!({ "pid": pid, "signal": signal.as_str(), "error": error })),
            );
        }
    }
}

pub fn resolve_context(deps: LifecycleDeps) -> LifecycleContext {
    let reattach_ports = match (deps.respawn, deps.reattach) {
        (Some(respawn), Some(reattach)) => Some(LifecycleReattachPorts { respawn, reattach }),
        _ => None,
    };
    let host_transport: Arc<dyn HostTransport> = deps
        .host_transport
        .clone()
        .unwrap_or_else(|| Arc::new(NoHostTransport));
    LifecycleContext {
        store: deps.store,
        registry: deps.registry,
        config: deps.config,
        now: deps.now.unwrap_or_else(|| {
            Arc::new(|| i64::try_from(crate::state::system_now_ms()).unwrap_or(i64::MAX))
        }),
        signaller: deps.signaller.unwrap_or_else(|| Arc::new(DefaultSignaller)),
        orphan_kill_delay_ms: deps
            .orphan_kill_delay_ms
            .unwrap_or(DEFAULT_ORPHAN_KILL_DELAY_MS),
        host_pid: deps
            .host_pid
            .unwrap_or_else(|| i64::from(std::process::id())),
        dequeue_pending: deps.dequeue_pending.unwrap_or_else(|| Arc::new(|_| {})),
        reattach_ports,
        reconcile_admission: deps.reconcile_admission.unwrap_or_default(),
        isolation: deps.isolation,
        isolation_probe: deps
            .isolation_probe
            .unwrap_or_else(isolation_core::process_owner_probe),
        host_session_probe: deps.host_session_probe.unwrap_or_else(|| {
            default_host_session_probe(host_transport.clone())
        }),
        host_session_close: deps
            .host_session_close
            .or_else(|| Some(default_host_session_closer(host_transport))),
        host_retry: deps
            .host_retry
            .unwrap_or_else(default_host_session_retry_policy),
        host_endpoint: deps
            .host_endpoint
            .unwrap_or_else(|| Arc::new(NoHostEndpoint)),
        host_close_timeout_ms: deps
            .host_close_timeout_ms
            .unwrap_or(DEFAULT_HOST_CLOSE_TIMEOUT_MS),
    }
}

impl LifecycleContext {
    pub fn now_iso(&self) -> String {
        crate::shared::iso_from_ms((self.now)())
    }

    pub(crate) fn delay_orphan_kill(&self) {
        if self.orphan_kill_delay_ms > 0 {
            std::thread::sleep(Duration::from_millis(self.orphan_kill_delay_ms));
        }
    }
}

pub(crate) fn is_terminal(status: TaskStatus) -> bool {
    status.is_terminal()
}
