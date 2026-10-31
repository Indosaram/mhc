use std::collections::HashMap;
pub const RPC_SESSION_IDLE_EVICTION_MS_ENV: &str = "SENPI_RPC_SESSION_IDLE_EVICTION_MS";
pub const RPC_HOST_EMPTY_EXIT_MS_ENV: &str = "SENPI_RPC_HOST_EMPTY_EXIT_MS";
pub const RPC_CLOSE_GRACE_MS_ENV: &str = "SENPI_RPC_CLOSE_GRACE_MS";
pub const DEFAULT_SESSION_IDLE_EVICTION_MS: f64 = 30. * 60_000.;
pub const DEFAULT_HOST_EMPTY_EXIT_MS: f64 = 15. * 60_000.;
#[derive(Debug, PartialEq)]
pub struct HostIdlePolicy {
    pub idle_eviction_ms: f64,
    pub empty_exit_ms: f64,
}
/// senpi `sessionMemory`: one `list_sessions` row's heap split. NATIVE-DIVERGENCE: a native host has no JS
/// main-thread heap, so `main_heap_bytes` is 0; the kernel figures come from the codemode kernel registry.
pub fn session_memory(
    kernels: &[maho_core::memory_report::kernel_registry_read::HostKernelMemoryRow],
    session_id: &str,
) -> serde_json::Value {
    let (count, bytes) = kernels
        .iter()
        .filter(|kernel| kernel.session_id == session_id)
        .fold((0u64, 0u64), |(count, bytes), kernel| {
            (count + 1, bytes + kernel.live_bytes)
        });
    serde_json::json!({"main_heap_bytes":0,"kernel_heap_bytes":bytes,"kernel_count":count})
}
/// senpi `createZeroSessionTrim` + the trim half of `startHostObservers`: a host that drops to zero sessions (or a
/// 2 GB+ host whose count drops) reports one `host_trimmed` record. NATIVE-DIVERGENCE: a native process has no
/// garbage collector to run, so `collect` answers `false` and the record reports `collected: false`.
pub fn create_zero_session_trim(
    emit: impl FnMut(serde_json::Value) + Send + 'static,
    sessions: impl FnMut() -> usize + Send + 'static,
) -> crate::host_zero_session_trim::ZeroSessionTrimmer {
    let started = std::time::Instant::now();
    crate::host_zero_session_trim::ZeroSessionTrimmer::new(
        crate::host_zero_session_trim::ZeroSessionTrimmerOptions {
            emit: Box::new(emit),
            sessions: Box::new(sessions),
            now: Box::new(|| crate::connection_handler::now_ms() as u64),
            monotonic_now: Box::new(move || started.elapsed().as_millis() as u64),
            read_footprint: Box::new(|| {
                let footprint = maho_core::process_footprint::read_own_footprint();
                crate::host_zero_session_trim::ProcessFootprint {
                    bytes: footprint.bytes,
                    measure: footprint.measure.as_str().to_owned(),
                }
            }),
            collect: Box::new(|| false),
        },
    )
}
pub enum HostMonitorSample {
    Lag(crate::loop_lag_watchdog::LagSample),
    Memory(crate::host_memory_sampler::MemorySample),
}
pub async fn run_host_monitors(
    env: &HashMap<String, String>,
    activity: &crate::session_attribution::SessionActivityRegistry,
    blocked: &std::sync::Mutex<crate::loop_blocked_time::LoopBlockedTime>,
    mut memory: impl FnMut() -> (u64, u64, u64),
    mut now: impl FnMut() -> f64,
    publish: impl FnMut(HostMonitorSample),
    stopped: tokio::sync::watch::Receiver<bool>,
) {
    let publish = std::sync::Mutex::new(publish);
    let mut lag = crate::loop_lag_watchdog::LoopLagWatchdog::new(env);
    let mut rss = crate::host_memory_sampler::HostMemorySampler::new(env);
    // senpi `onSessionActivityStart`: attributed work starting returns the watchdog to its fast tick at once.
    let wake = std::sync::Arc::new(tokio::sync::Notify::new());
    let unsubscribe = crate::session_attribution::on_session_activity_start({
        let wake = wake.clone();
        move || wake.notify_one()
    });
    let stall = lag.run_shared_with_wake(
        activity,
        blocked,
        &mut now,
        |sample| {
            publish
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)(
                HostMonitorSample::Lag(sample)
            );
        },
        stopped.clone(),
        Some(wake),
    );
    let pressure = rss.run_until_stopped(
        &mut memory,
        |sample| {
            publish
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)(
                HostMonitorSample::Memory(sample),
            );
        },
        stopped,
    );
    tokio::join!(stall, pressure);
    unsubscribe();
}
/// The Windows presenter control pipe (senpi `presenterPipePath`). Its name comes from its own random value, never
/// the host's public protocol instanceId, so nothing can learn it before the host listens and squat it.
pub fn presenter_pipe_path() -> String {
    format!(r"\\.\pipe\senpi-presenter-{}", uuid::Uuid::new_v4())
}
pub fn resolve_host_idle_policy(
    env: &HashMap<String, String>,
    idle_eviction_ms: Option<f64>,
    empty_exit_ms: Option<f64>,
) -> HostIdlePolicy {
    HostIdlePolicy {
        idle_eviction_ms: idle_eviction_ms
            .or_else(|| {
                crate::host_lifecycle::parse_idle_exit_ms(
                    env.get(RPC_SESSION_IDLE_EVICTION_MS_ENV)
                        .map(String::as_str),
                )
            })
            .unwrap_or(DEFAULT_SESSION_IDLE_EVICTION_MS),
        empty_exit_ms: empty_exit_ms
            .or_else(|| {
                crate::host_lifecycle::parse_idle_exit_ms(
                    env.get(RPC_HOST_EMPTY_EXIT_MS_ENV).map(String::as_str),
                )
            })
            .unwrap_or(DEFAULT_HOST_EMPTY_EXIT_MS),
    }
}
/**
 * Creates the runtime an `open_session` needs. Supplied by the caller: the DEFAULT factory that
 * builds one from CLI configuration is the owner-blocked piece (maho-core needs a canonical
 * `createAgentSessionRuntime`; which `CliRuntimeConfiguration` to build from is maho-cli's).
 * This seam is the contract lane 35 consumes.
 */
pub type HostRuntimeFactory = std::sync::Arc<
    dyn Fn(
            crate::session_registry::RpcSessionLaunchProfile,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<
                        Output = Result<
                            maho_core::agent_session_runtime::AgentSessionRuntime,
                            String,
                        >,
                    > + Send,
            >,
        > + Send
        + Sync,
>;
/// Everything one shared host needs to serve sessions.
pub struct HostCoreOptions {
    pub agent_dir: std::path::PathBuf,
    pub cwd: String,
    pub create_runtime: HostRuntimeFactory,
    pub capabilities: Vec<String>,
    pub close_grace_ms: u64,
}
/// Process-local lifecycle owner for multi-session RPC runtimes (senpi's `createHostCore`
/// result): one registry, one binding per session, and one event fanout every connection is
/// registered with (senpi `SessionEventWriter`'s `SessionEventFanout`).
pub struct HostCore {
    pub registry: tokio::sync::Mutex<crate::session_registry::RpcSessionRegistry>,
    pub default_writer: std::sync::Arc<crate::session_event_writer::SessionWriterActor>,
    pub attachments: std::sync::Mutex<crate::session_command_router::ConnectionAttachments>,
    /// Per-connection delivery: attachment sets, capabilities, late-joiner snapshots.
    pub fanout: std::sync::Arc<std::sync::Mutex<crate::session_event_fanout::SessionEventFanout>>,
    /// Loop time the stall watchdog recorded; socket sinks extend their drain deadline by it.
    pub blocked: std::sync::Arc<std::sync::Mutex<crate::loop_blocked_time::LoopBlockedTime>>,
    widths: std::sync::Mutex<crate::session_command_router::SharedSessionWidths>,
    /// Session attribution the stall watchdog blames a blocked loop on (senpi `runWithSessionAttribution`).
    pub activity: crate::session_attribution::SessionActivityRegistry,
    /// Raised by the memory sampler: idle sessions park at half the window (senpi `setMemoryPressure`).
    memory_pressure: std::sync::atomic::AtomicBool,
    /// `set_client_info` capabilities a connection sent before it attached (senpi `pendingCapabilities`).
    pending_capabilities: std::sync::Mutex<std::collections::BTreeMap<String, Vec<String>>>,
    /// Each connection's opens still in flight (senpi `opensByConnection`): an open's barrier settles when it answers.
    opens_by_connection: std::sync::Mutex<OpensByConnection>,
    bindings: tokio::sync::Mutex<
        std::collections::BTreeMap<
            String,
            std::sync::Arc<crate::session_binding::RpcSessionBinding>,
        >,
    >,
    options: HostCoreOptions,
    /// Resolved `--tool-runtime` (senpi `MultiSessionHostOptions.toolRuntime`); absent means `in-process`.
    /// Advertised as `tool_runtime:<value>`.
    tool_runtime: crate::host_capabilities::ToolRuntimeKind,
    /// Set only where a SIGUSR1 drain handler is actually installed (senpi `runSocketHost`): the
    /// `generation_handoff` capability is the proof a handler exists and is advertised with it.
    handoff_enabled: std::sync::atomic::AtomicBool,
    drain: std::sync::Mutex<crate::session_command_router::HandoffDrain>,
    /// Sessions a drain has already claimed, so a later sweep never parks one twice.
    handoff_closed: std::sync::Mutex<std::collections::BTreeSet<String>>,
    /// In-flight parks, so the drain exit waits for them (senpi `handoffParks`).
    handoff_parks: std::sync::atomic::AtomicUsize,
    /// Set when the host runs every session in its own worker process (senpi `createHostCore` selects
    /// `WorkerSessionRegistry` when a `workerConfiguration` is passed); `None` is the in-process registry.
    workers: Option<std::sync::Arc<crate::worker_session_registry::WorkerSessionRegistry>>,
    /// Who each connection speaks for (senpi `SessionCommandRouter.connectionRoles`, senpi#3811).
    connection_roles: crate::session_binding::SharedConnectionRoles,
    /// `<daemonDir>` the handoff-parked record lives under (senpi `handoverParkedDir`); set once a socket host
    /// knows its endpoint.
    handover_parked_dir: std::sync::OnceLock<std::path::PathBuf>,
    /// Requests in flight per session routing handle, the release itself included (senpi `activeRequests`).
    active_requests: std::sync::Mutex<std::collections::BTreeMap<String, usize>>,
    /// Woken whenever a request settles (senpi `requestSettledListeners`).
    request_settled: tokio::sync::Notify,
    /// The writer's MCP presenter state (senpi `SessionEventWriter` presenter half); enabled only by enforcement.
    pub presenters: std::sync::Arc<crate::session_event_writer::SessionMcpPresenters>,
    /// `--mcp-presenter-fd 4` (senpi `MultiSessionHostOptions.presenterBootstrapFd`), adopted (close-on-exec) as
    /// soon as the host is configured so no child this host spawns inherits it.
    presenter_bootstrap:
        std::sync::Mutex<Option<crate::mcp_presenter_bootstrap::AdoptedPresenterBootstrap>>,
    /// The private control endpoint a socket host opens when it was launched with enforcement and a bootstrap.
    presenter_control:
        std::sync::Mutex<Option<std::sync::Arc<crate::mcp_presenter_control::McpPresenterControl>>>,
    /// The conditional idle handover a POSIX socket host can perform, with its endpoint and loaded runtime (senpi
    /// `HostCoreGate`); unset, `begin_handover` is refused `handover_unsupported`.
    handover: std::sync::OnceLock<HostHandoverGate>,
    /// The public endpoint clients dial (senpi `hostSessionContext(...).host_socket`), set once a socket host knows it:
    /// a hosted session's control endpoint is this socket.
    public_socket: std::sync::OnceLock<String>,
    /// senpi `HostCoreHooks.runtimeBuildId`: the runtime this host loaded at startup, reported by `get_protocol_info`
    /// on every platform (the idle handover that also reads it is POSIX-only).
    runtime_build_id: std::sync::OnceLock<String>,
    /// Requests in flight that carry no session (an open being admitted): the idle judgement waits for them.
    sessionless_requests: std::sync::atomic::AtomicUsize,
    /// senpi `HostCoreGate.answering`: `begin_handover` requests whose answer has not been delivered yet.
    handover_answering: std::sync::atomic::AtomicUsize,
    /// Woken whenever a `begin_handover` answer was delivered.
    handover_answered: tokio::sync::Notify,
}
/// senpi `HostCoreGate`'s state: the idle handover, this host's endpoint (a `begin_handover` naming another agent
/// dir is refused), and the runtime build id `get_protocol_info` reports.
pub struct HostHandoverGate {
    pub handover: crate::host_idle_handover::HostIdleHandover,
    pub endpoint: crate::host_handover_wire::HostEndpoint,
    pub runtime_build_id: Option<String>,
}
/// A refused open: the wire message, and the registry code when one carried it (`session_path_in_use`).
struct OpenRefusal {
    message: String,
    code: Option<String>,
    /// `errorData` a client acts on (`session_output_overflow { sessionId, sessionPath, action }`).
    data: Option<serde_json::Value>,
}
impl OpenRefusal {
    fn plain(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            code: None,
            data: None,
        }
    }
    /// senpi router `error(id, command, code(cause), detail(cause))`: the registry's detail (who holds a path, when
    /// to retry) rides as `errorData`.
    fn from_registry(error: &crate::session_registry::RpcSessionRegistryError) -> Self {
        Self {
            message: error.to_string(),
            code: Some(error.code.clone()),
            data: error.detail.clone(),
        }
    }
    fn sealed(data: serde_json::Value) -> Self {
        Self {
            message: crate::rpc_types::RPC_ERROR_SESSION_OUTPUT_OVERFLOW.to_owned(),
            code: Some(crate::rpc_types::RPC_ERROR_SESSION_OUTPUT_OVERFLOW.to_owned()),
            data: Some(data),
        }
    }
}
/// A sessionless request in flight, until dropped.
struct SessionlessGuard<'a>(&'a std::sync::atomic::AtomicUsize);
impl Drop for SessionlessGuard<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}
/// senpi `HostCoreGate.intercept`'s `answering` entry: held until the `begin_handover` answer was delivered.
struct HandoverAnswering<'a>(&'a HostCore);
impl<'a> HandoverAnswering<'a> {
    fn new(core: &'a HostCore) -> Self {
        core.handover_answering
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Self(core)
    }
}
impl Drop for HandoverAnswering<'_> {
    fn drop(&mut self) {
        self.0
            .handover_answering
            .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
        self.0.handover_answered.notify_waiters();
    }
}
/// Re-checks a pending handover once a routed request settles (senpi `handover?.observe()` in `dispatch`).
struct HandoverObserve<'a>(Option<&'a crate::host_idle_handover::HostIdleHandover>);
impl Drop for HandoverObserve<'_> {
    fn drop(&mut self) {
        if let Some(handover) = self.0 {
            handover.observe();
        }
    }
}
/// Counts one routed request against its session until dropped.
struct RequestGuard<'a> {
    core: &'a HostCore,
    session_id: String,
}
impl Drop for RequestGuard<'_> {
    fn drop(&mut self) {
        {
            let mut active = self
                .core
                .active_requests
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            match active.get_mut(&self.session_id) {
                Some(count) if *count > 1 => *count -= 1,
                _ => {
                    active.remove(&self.session_id);
                }
            }
        }
        self.core.request_settled.notify_waiters();
    }
}
impl HostCore {
    pub fn new(
        options: HostCoreOptions,
        default_writer: std::sync::Arc<crate::session_event_writer::SessionWriterActor>,
    ) -> Self {
        let presenters =
            std::sync::Arc::<crate::session_event_writer::SessionMcpPresenters>::default();
        let fanout = std::sync::Arc::new(std::sync::Mutex::new(
            crate::session_event_fanout::SessionEventFanout::default(),
        ));
        // senpi `fanout.onConnectionClosed`: a connection reaped on a failed queue loses its presenter state. The
        // detach never re-enters the fanout: a revoked grant is no longer current, so no private frame is sent.
        let reaped = std::sync::Arc::downgrade(&presenters);
        fanout
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .set_on_connection_closed(std::sync::Arc::new(move |id| {
                if let Some(presenters) = reaped.upgrade() {
                    presenters.unregister_connection(id);
                }
            }));
        Self {
            registry: tokio::sync::Mutex::new(
                crate::session_registry::RpcSessionRegistry::default(),
            ),
            default_writer,
            attachments: Default::default(),
            fanout,
            blocked: Default::default(),
            widths: Default::default(),
            activity: Default::default(),
            memory_pressure: std::sync::atomic::AtomicBool::new(false),
            pending_capabilities: Default::default(),
            opens_by_connection: Default::default(),
            bindings: tokio::sync::Mutex::new(std::collections::BTreeMap::new()),
            options,
            tool_runtime: crate::host_capabilities::ToolRuntimeKind::InProcess,
            handoff_enabled: std::sync::atomic::AtomicBool::new(false),
            drain: std::sync::Mutex::new(crate::session_command_router::HandoffDrain::default()),
            handoff_closed: std::sync::Mutex::new(std::collections::BTreeSet::new()),
            handoff_parks: std::sync::atomic::AtomicUsize::new(0),
            workers: None,
            connection_roles: Default::default(),
            handover_parked_dir: std::sync::OnceLock::new(),
            active_requests: Default::default(),
            request_settled: tokio::sync::Notify::new(),
            presenters,
            presenter_bootstrap: std::sync::Mutex::new(None),
            presenter_control: std::sync::Mutex::new(None),
            handover: std::sync::OnceLock::new(),
            public_socket: std::sync::OnceLock::new(),
            runtime_build_id: std::sync::OnceLock::new(),
            sessionless_requests: std::sync::atomic::AtomicUsize::new(0),
            handover_answering: std::sync::atomic::AtomicUsize::new(0),
            handover_answered: tokio::sync::Notify::new(),
        }
    }
    /// Installs the conditional idle handover (senpi `runSocketHost` `canHandOver`): the operation judges idleness
    /// over this core and performs the successor launch with this host's successor enrollment channel
    /// ([`Self::presenter_successor_bootstrap`]). The first call wins.
    pub fn enable_idle_handover(
        self: &std::sync::Arc<Self>,
        endpoint: crate::host_handover_wire::HostEndpoint,
        runtime_build_id: String,
    ) {
        let idle = std::sync::Arc::downgrade(self);
        let identity_build = runtime_build_id.clone();
        let performer = std::sync::Arc::downgrade(self);
        let perform_endpoint = endpoint.clone();
        let handover = crate::host_idle_handover::HostIdleHandover::new(
            crate::host_idle_handover::HostIdleHandoverOptions {
                identity: Box::new(move || crate::host_idle_handover::HostIdentityView {
                    instance_id: crate::protocol_identity::host_instance_id().to_owned(),
                    generation: crate::protocol_identity::host_generation(
                        &std::env::vars().collect(),
                    ),
                    runtime_build_id: Some(identity_build.clone()),
                }),
                is_idle: Box::new(move || idle.upgrade().is_some_and(|core| core.is_host_idle())),
                perform: Box::new(move |request| {
                    let core = performer.upgrade();
                    let endpoint = perform_endpoint.clone();
                    Box::pin(async move {
                        let bootstrap = match &core {
                            Some(core) => core.presenter_successor_bootstrap().await,
                            None => None,
                        };
                        crate::host_handover_wire::perform_idle_handover(
                            request,
                            &endpoint.socket,
                            &endpoint.agent_dir,
                            bootstrap,
                        )
                        .await
                    })
                }),
                poll_ms: None,
                log: Some(Box::new(|line: &str| eprintln!("senpi rpc host: {line}"))),
            },
        );
        let _ = self.handover.set(HostHandoverGate {
            handover,
            endpoint,
            runtime_build_id: Some(runtime_build_id),
        });
    }
    /// senpi `HostCoreHooks.runtimeBuildId`: reported by `get_protocol_info`; the first call wins.
    pub fn set_runtime_build_id(&self, runtime_build_id: String) {
        let _ = self.runtime_build_id.set(runtime_build_id);
    }
    /// senpi `isHostIdle` over this core: no sessionless request in flight and every session settled.
    ///
    /// ADAPTATION: senpi's registry is synchronous; the in-process registry here is an async lock, and a judgement
    /// made while another task holds it (an open, a close, a sweep) answers "not idle". That is the same answer the
    /// work in progress would give, and the idle handover re-judges on its next poll and after every request.
    fn is_host_idle(&self) -> bool {
        let sessionless = self
            .sessionless_requests
            .load(std::sync::atomic::Ordering::SeqCst)
            > 0;
        let in_flight = |id: &str| self.active_request_count(id) > 0;
        match &self.workers {
            Some(workers) => {
                let rows = workers.list_sessions(true);
                let candidates = rows
                    .iter()
                    .filter_map(|row| row["sessionId"].as_str())
                    .map(|id| crate::session_drain::DrainCandidate {
                        session_id: id,
                        session_path: None,
                        worker_handoff_busy: workers
                            .worker(id)
                            .is_some_and(|worker| worker.handoff_busy()),
                        activity: None,
                    })
                    .collect();
                crate::host_handover_wire::is_host_idle(candidates, sessionless, in_flight)
            }
            None => {
                let Ok(registry) = self.registry.try_lock() else {
                    return false;
                };
                let rows = registry.list_sessions(true);
                let candidates = rows
                    .iter()
                    .filter_map(|row| row["sessionId"].as_str())
                    .map(|id| crate::session_drain::DrainCandidate {
                        session_id: id,
                        session_path: None,
                        worker_handoff_busy: false,
                        activity: registry
                            .get(id)
                            .and_then(|entry| entry.runtime.as_ref())
                            .map(|runtime| Ok(runtime.session().activity_snapshot())),
                    })
                    .collect();
                crate::host_handover_wire::is_host_idle(candidates, sessionless, in_flight)
            }
        }
    }
    /// Names the daemon directory the handoff-parked record lives under (senpi `handoverParkedDir`); the first
    /// call wins.
    pub fn set_handover_parked_dir(&self, dir: std::path::PathBuf) {
        let _ = self.handover_parked_dir.set(dir);
    }
    /// senpi `createHostCore` `pathReservations`: the cross-generation claims the in-process registry publishes for
    /// its sessions' files, in the daemon directory of the endpoint clients dial (a told
    /// `SENPI_RPC_HOST_DAEMON_DIR` wins). Two generations of this daemon can be alive at once during a handoff; these
    /// claims keep them off one session file. A worker-runtime host keeps its own claims per worker.
    pub async fn install_endpoint_reservations(&self, endpoint_socket: Option<&str>) {
        if let Some(socket) = endpoint_socket {
            let _ = self.public_socket.set(socket.to_owned());
        }
        if self.workers.is_some() {
            return;
        }
        let reservations = crate::host_reservations::create_endpoint_reservations(
            &self.options.agent_dir,
            endpoint_socket,
            crate::protocol_identity::host_instance_id().to_owned(),
            |message| eprintln!("senpi rpc host: {message}"),
        )
        .map(|reservations| reservations.with_public_socket(endpoint_socket.map(str::to_owned)));
        self.registry
            .lock()
            .await
            .set_path_reservations(reservations);
    }
    /// senpi `HostCoreGate.answered`: settles once every `begin_handover` being answered has its answer. A host idle at
    /// once drains the moment its successor takes the socket, and its shutdown waits here so the caller that asked is
    /// told the outcome instead of reading a closed connection.
    pub async fn handover_answered(&self) {
        loop {
            // Registered before the count is read, so a delivery between the read and the wait still wakes it.
            let answered = self.handover_answered.notified();
            tokio::pin!(answered);
            answered.as_mut().enable();
            if self
                .handover_answering
                .load(std::sync::atomic::Ordering::SeqCst)
                == 0
            {
                return;
            }
            answered.await;
        }
    }
    /// senpi shutdown `handover?.dispose()`: a pending idle handover stops its timer with the host.
    pub fn dispose_handover(&self) {
        if let Some(gate) = self.handover.get() {
            gate.handover.dispose();
        }
    }
    /// Whether a handoff drain is running (senpi `router.draining`).
    pub fn is_draining(&self) -> bool {
        self.drain
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_draining()
    }
    fn begin_request(&self, session_id: Option<&str>) -> Option<RequestGuard<'_>> {
        let session_id = session_id?.to_owned();
        *self
            .active_requests
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(session_id.clone())
            .or_insert(0) += 1;
        Some(RequestGuard {
            core: self,
            session_id,
        })
    }
    fn active_request_count(&self, session_id: &str) -> usize {
        self.active_requests
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(session_id)
            .copied()
            .unwrap_or(0)
    }
    /// The successor's reopen of a handoff-parked session (senpi `reopenParked`): the same open as an
    /// `open_session`, with no requesting connection and nothing to reply to.
    pub async fn reopen_parked(
        &self,
        entry: crate::host_handover_parked::HandoverParkedEntry,
    ) -> Result<(), crate::host_handover_reopen::ReopenError> {
        let refused = |message: String, code: Option<String>| {
            crate::host_handover_reopen::ReopenError { code, message }
        };
        let mut wire = crate::host_handover_parked_open::parked_open_command(&entry);
        wire["attended"] = false.into();
        let command = serde_json::from_value::<crate::rpc_types::RpcCommand>(wire)
            .map_err(|error| refused(format!("open_failed: {error}"), None))?;
        self.open_session_inner(None, &command.body, entry.context_overlay.as_ref())
            .await
            .map(|_| ())
            .map_err(|refusal| refused(refusal.message, refusal.code))
    }
    /// The park record of a retained session, `None` for anything a successor must not reopen.
    fn park_record(
        &self,
        retain: bool,
        session_path: Option<&str>,
        durable_session_id: Option<&str>,
        profile: &crate::session_registry::RpcSessionLaunchProfile,
        overlay: Option<&crate::session_context_patch::SessionContextOverlay>,
    ) -> Option<crate::host_handover_parked::HandoverParkedEntry> {
        self.handover_parked_dir.get()?;
        let session_path = session_path.filter(|_| retain)?;
        let overlay = overlay.cloned().map(serde_json::Value::Object);
        let mut entry = crate::host_handover_parked_open::parked_entry_from_profile(
            &crate::host_handover_parked_open::ParkedSession {
                session_path,
                durable_session_id,
                context_overlay: overlay.as_ref(),
            },
            Some(profile),
            &crate::host_handover_parked_open::ParkedLaunchExtras::default(),
            &[],
        );
        entry.parked_by = Some(crate::protocol_identity::host_instance_id().to_owned());
        Some(entry)
    }
    /// Writes one park record, awaited before the drain proceeds (senpi `recordHandoverParkedSession`).
    async fn write_park_record(
        &self,
        entry: Option<crate::host_handover_parked::HandoverParkedEntry>,
    ) {
        let (Some(entry), Some(dir)) = (entry, self.handover_parked_dir.get()) else {
            return;
        };
        if let Err(error) =
            crate::host_handover_parked::record_handover_parked_session(dir, &entry).await
        {
            eprintln!(
                "senpi rpc handover park record {} failed: {error}",
                entry.session_path
            );
        }
    }
    /// Selects where heavy tool work runs (senpi `toolRuntime`); the host advertises it in `get_protocol_info`.
    pub fn with_tool_runtime(
        mut self,
        tool_runtime: crate::host_capabilities::ToolRuntimeKind,
    ) -> Self {
        self.tool_runtime = tool_runtime;
        self
    }
    /// Host deployment policy; never derived from client capabilities or project settings (senpi
    /// `MultiSessionHostOptions.mcpPresenterEnforcement`): private MCP forms are enforced and
    /// `mcp_form_presenter_v1` is advertised.
    pub fn with_mcp_presenter_enforcement(self, enforce: bool) -> Self {
        if enforce {
            self.presenters.enable();
        }
        self
    }
    /// `--mcp-presenter-fd 4`: the inherited enrollment channel a socket host opens its presenter control with.
    /// The descriptor is adopted here: it is close-on-exec from now on and closed when the host drops it.
    pub fn with_presenter_bootstrap_fd(self, fd: Option<i32>) -> Self {
        if let Some(adopted) =
            fd.and_then(crate::mcp_presenter_bootstrap::adopt_presenter_bootstrap_fd)
        {
            *self
                .presenter_bootstrap
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(adopted);
        }
        self
    }
    /// A deferred handover's successor enrollment channel (senpi `presenterControl?.successorBootstrap()`).
    pub async fn presenter_successor_bootstrap(
        &self,
    ) -> Option<crate::mcp_presenter_wire::BoxedPrivateStream> {
        let control = self
            .presenter_control
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()?;
        control.successor_bootstrap().await
    }
    /// senpi `runSocketHost` presenter startup: with a bootstrap descriptor and enforcement, open the private
    /// control endpoint in a private temp dir and enroll over the descriptor; otherwise the descriptor is dropped.
    /// Bootstrap is optional authority, never a reason to fail the host.
    #[cfg(unix)]
    async fn start_presenter_control(
        self: &std::sync::Arc<Self>,
    ) -> Option<tempfile_dir::PresenterDir> {
        let adopted = self
            .presenter_bootstrap
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()?;
        let bootstrap = crate::mcp_presenter_bootstrap::presenter_bootstrap_stream_from(adopted)?;
        if !self.presenters.enabled() {
            drop(bootstrap);
            return None;
        }
        let dir = tempfile_dir::PresenterDir::create()?;
        let control = std::sync::Arc::new(crate::mcp_presenter_control::McpPresenterControl::new(
            std::sync::Arc::new(PresenterWriter {
                presenters: std::sync::Arc::clone(&self.presenters),
                fanout: std::sync::Arc::clone(&self.fanout),
            }),
            self.presenters.enable(),
            &dir.path.join("control.sock").to_string_lossy(),
        ));
        if control.listen().await.is_err() {
            control.dispose();
            return None;
        }
        control.bootstrap(bootstrap);
        *self
            .presenter_control
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(control);
        Some(dir)
    }
    fn dispose_presenter_control(&self) {
        let control = self
            .presenter_control
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(control) = control {
            control.dispose();
        }
    }
    /// The tool runtime this host advertises.
    pub fn tool_runtime(&self) -> crate::host_capabilities::ToolRuntimeKind {
        self.tool_runtime
    }
    /// Selects the worker runtime: every opened session runs in an isolated worker process.
    pub fn with_worker_registry(
        mut self,
        workers: crate::worker_session_registry::WorkerSessionRegistry,
    ) -> Self {
        self.workers = Some(std::sync::Arc::new(workers));
        self
    }
    pub fn uses_worker_runtime(&self) -> bool {
        self.workers.is_some()
    }
    /// Sessions the host holds in whichever registry serves them.
    pub async fn session_count(&self) -> usize {
        match &self.workers {
            Some(workers) => workers.size(),
            None => self.registry.lock().await.size(),
        }
    }
    /// Registers one connection's sink with the fanout (senpi `SessionEventWriter.registerConnection`).
    pub fn register_connection(
        &self,
        connection: &str,
        actor: crate::socket_event_fanout::SocketEventSinkActor,
    ) {
        self.fanout
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .register(connection, actor);
        // A private frame is written raw on the connection's control lane, never through a session queue.
        let fanout = std::sync::Arc::downgrade(&self.fanout);
        let id = connection.to_owned();
        self.presenters.register_connection(
            connection,
            std::sync::Arc::new(move |frame| {
                fanout.upgrade().is_some_and(|fanout| {
                    fanout
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .enqueue_raw(&id, frame)
                })
            }),
        );
    }
    /// Unregisters a connection's sink, its capabilities and its render widths.
    pub fn unregister_connection(&self, connection: &str) {
        self.presenters.unregister_connection(connection);
        self.fanout
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .unregister(connection);
        self.pending_capabilities
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(connection);
        self.widths
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear_connection(connection);
        self.connection_roles
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .release(connection);
    }
    /// Advertises `generation_handoff` only once a SIGUSR1 handler is actually installed (senpi
    /// `runSocketHost`): the capability IS the proof a handler exists, and a host without it is
    /// never signalled - SIGUSR1 would simply terminate it, sessions and all.
    pub fn enable_handoff(&self) {
        self.handoff_enabled
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }
    /// Host capabilities first, then the launch's client capabilities, deduplicated in order
    /// (senpi `SessionCommandRouter.dispatch` `get_protocol_info`).
    fn advertised_capabilities(&self) -> Vec<String> {
        use crate::custom_capability::*;
        let mut capabilities = Vec::<String>::new();
        // senpi `multiSessionHostCapabilities`: `mcp_form_presenter_v1` only when this deployment enforces presenters.
        let presenter = self
            .presenters
            .enabled()
            .then_some(crate::host_capabilities::MCP_FORM_PRESENTER_CAPABILITY);
        // senpi `hostToolRuntimeCapabilities`: the host's own placement first; a negotiated `tool_runtime:*` never overrides it.
        let tool_runtime = crate::host_capabilities::host_tool_runtime_capabilities(
            self.tool_runtime,
            &self.options.capabilities,
        );
        for capability in [
            "multi_session",
            SNAPSHOT_CURSOR_CAPABILITY,
            UI_ANSWER_LEDGER_CAPABILITY,
        ]
        .into_iter()
        .chain(presenter)
        .chain([
            ATTACH_SESSION_CAPABILITY,
            RELEASE_ATTACHMENT_CAPABILITY,
            // `release_session` already counts only OTHER connections' attachments (`other_attachments`).
            RELEASE_CALLER_AWARE_CAPABILITY,
            RECLAIM_SESSION_CAPABILITY,
            AUTO_TITLE_SESSIONS_CAPABILITY,
            MEDIA_PLACEHOLDERS_CAPABILITY,
            crate::rpc_types::STEER_WHEN_IDLE_CAPABILITY,
            crate::rpc_types::QUEUED_MESSAGE_REVISION_CAPABILITY,
            crate::rpc_types::USER_ACTIVITY_CAPABILITY,
            SESSION_HELD_CAPABILITY,
            RESUME_SESSION_CAPABILITY,
            ATTENDED_CAPABILITY,
            // `get_state`/`get_entries` marked `observe` never attach, and history reads page backward too (senpi#4267).
            OBSERVE_READS_CAPABILITY,
            ENTRIES_TAIL_CAPABILITY,
            RETAIN_ON_DISCONNECT_CAPABILITY,
            SESSION_CONTEXT_CAPABILITY,
            SESSION_STATUS_CAPABILITY,
            DIALOG_OBSERVER_CAPABILITY,
            SESSION_KIND_CAPABILITY,
            AUTO_TITLE_PER_SESSION_CAPABILITY,
            DURABLE_SESSION_ID_CAPABILITY,
            crate::host_capabilities::UNLOAD_SESSION_CAPABILITY,
        ])
        .chain(std::iter::once(
            crate::socket_peer_recovery::EVENT_RESYNC_CAPABILITY,
        ))
        .chain(tool_runtime.iter().map(String::as_str))
        {
            if !capabilities.iter().any(|existing| existing == capability) {
                capabilities.push(capability.to_owned());
            }
        }
        if self.handover.get().is_some() {
            capabilities
                .push(crate::host_handover_request::RUNTIME_IDENTITY_HANDOVER_CAPABILITY.into());
        }
        if self
            .handoff_enabled
            .load(std::sync::atomic::Ordering::SeqCst)
            && !capabilities
                .iter()
                .any(|capability| capability == crate::host_decision::GENERATION_HANDOFF_CAPABILITY)
        {
            capabilities.push(crate::host_decision::GENERATION_HANDOFF_CAPABILITY.into());
        }
        capabilities
    }
    /// Broadcasts one host-lifecycle record to every registered connection (senpi
    /// `SessionEventWriter.broadcastHostRecord`); a host with no connection answers on its default writer.
    pub fn broadcast_record(&self, record: &serde_json::Value) {
        let stdio = self
            .fanout
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .broadcast_host_record(record);
        match stdio {
            Ok(Some(record)) => {
                let _ = self.default_writer.enqueue("", record);
            }
            Ok(None) => {}
            Err(error) => eprintln!("senpi rpc host record failed: {error}"),
        }
    }
    /// The roles `connection` explicitly declared, handed to a worker isolate that keeps its own registry.
    fn declared_roles(&self, connection: Option<&str>) -> Option<Vec<String>> {
        self.connection_roles
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .roles_of(connection)
            .map(<[String]>::to_vec)
    }
    /// Routes one session record through the fanout (senpi `SessionEventWriter.enqueue`).
    fn route(&self, session_id: &str, owner: Option<&str>, record: serde_json::Value) {
        route_session_record(
            &self.fanout,
            &self.default_writer,
            session_id,
            owner,
            record,
        );
    }
    /// Re-reads one session's registry half into the status source (senpi `registryStatusSource` reads it live);
    /// a handle no registry holds any more leaves the source.
    async fn sync_status(&self, session_id: &str) {
        use crate::session_status_source::{RegistryEntryFields, StatusRuntime, entry_view};
        let projected = match &self.workers {
            Some(workers) => workers.with_entry(session_id, |entry| {
                (
                    entry_view(RegistryEntryFields {
                        state: entry.close.state,
                        kind: entry.identity.kind,
                        cwd: &entry.cwd,
                        attachments: entry.close.attachments,
                        durable_session_id: entry.durable_session_id.clone(),
                        session_path: entry.session_path.clone(),
                        context: &entry.identity.context,
                        overlay: entry.context_overlay.as_ref(),
                    }),
                    StatusRuntime::Worker(std::sync::Arc::clone(&entry.worker)),
                )
            }),
            None => self.registry.lock().await.get(session_id).map(|entry| {
                // A switch replaces the session in place, so its live cwd is the session's, not the runtime's.
                let cwd = entry.runtime.as_ref().map_or_else(
                    || entry.profile.runtime.cwd.clone(),
                    |runtime| runtime.session().cwd(),
                );
                (
                    entry_view(RegistryEntryFields {
                        state: entry.close.state,
                        kind: entry.identity.kind,
                        cwd: &cwd,
                        attachments: entry.close.attachments,
                        durable_session_id: entry.durable_session_id.clone(),
                        session_path: entry.session_path.clone(),
                        context: &entry.identity.context,
                        overlay: entry.context_overlay.as_ref(),
                    }),
                    entry
                        .runtime
                        .as_ref()
                        .map_or(StatusRuntime::None, |runtime| {
                            StatusRuntime::Session(std::sync::Arc::clone(runtime))
                        }),
                )
            }),
        };
        let fanout = self
            .fanout
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut hub = fanout
            .status
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match projected {
            Some((entry, runtime)) => hub.source.upsert(session_id, entry, runtime),
            None => hub.source.remove(session_id),
        }
    }
    /// Syncs the session's registry half, then publishes one status change (senpi `writer.status.publish`).
    async fn publish_status(
        &self,
        session_id: &str,
        cause: crate::session_status::SessionStatusCause,
        by: crate::session_status::SessionStatusActor,
        context_only: bool,
    ) {
        self.sync_status(session_id).await;
        let fanout = self
            .fanout
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let status = std::sync::Arc::clone(&fanout.status);
        let mut guard = status
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let hub = &mut *guard;
        hub.stream
            .publish(&hub.source, &*fanout, session_id, cause, by, context_only);
    }
    /// `subscribe_sessions` (senpi `answerSubscribeSessions` + the router's two-hop activation): the answer carrying
    /// the `seq 0` snapshot is on the connection's FIFO before the subscription goes live.
    fn subscribe_sessions(&self, connection: Option<&str>, request: &serde_json::Value) {
        let fanout = self
            .fanout
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let status = std::sync::Arc::clone(&fanout.status);
        let mut guard = status
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let hub = &mut *guard;
        let answered = crate::session_status_commands::answer_subscribe_sessions(
            request,
            connection,
            &mut hub.stream,
            &hub.source,
            &*fanout,
        );
        let (Some(connection), Some(subscription)) = (connection, answered.activate) else {
            drop(guard);
            drop(fanout);
            return self.respond_value(connection, "", answered.answer);
        };
        // Both locks are held across the answer and the activation, so no delta can land between them.
        match fanout.enqueue_control(connection, &answered.answer) {
            Ok(true) => hub.stream.activate(&*fanout, &subscription),
            Ok(false) | Err(_) => {
                hub.stream.unsubscribe(connection, &subscription);
                eprintln!("senpi rpc subscribe answer could not be delivered to {connection}");
            }
        }
    }
    /// `unsubscribe_sessions` (senpi `answerUnsubscribeSessions`).
    fn unsubscribe_sessions(&self, connection: Option<&str>, request: &serde_json::Value) {
        let answer = {
            let fanout = self
                .fanout
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let mut status = fanout
                .status
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            crate::session_status_commands::answer_unsubscribe_sessions(
                request,
                connection,
                &mut status.stream,
            )
        };
        self.respond_value(connection, "", answer);
    }
    /// Drops every per-connection record of a handle the host ended (senpi `forgetSessionOwnership`).
    fn forget_session(&self, session_id: &str) {
        {
            let fanout = self
                .fanout
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let mut hub = fanout
                .status
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            hub.source.remove(session_id);
            hub.stream.forget(session_id);
        }
        self.presenters.dispose_session(session_id);
        self.fanout
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .forget_session(session_id);
        self.attachments
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .forget_session(session_id);
        self.widths
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .forget_session(session_id);
        self.default_writer.forget_session(session_id);
    }
    /// Starts the drain this host parks itself with (senpi `SessionCommandRouter.beginDrain`).
    pub async fn begin_drain(self: &std::sync::Arc<Self>) {
        self.drain
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .begin();
        let _ = self.drain_sweep().await;
    }
    /// One drain pass (senpi `sweepDrain`): park every open session whose work has settled, then
    /// report whether the host holds nothing and the generation may leave. A busy accepted open
    /// blocks the pass exactly as senpi's `activeRequests.has(undefined)` guard does.
    pub async fn drain_sweep(self: &std::sync::Arc<Self>) -> bool {
        if !self
            .drain
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_draining()
        {
            return false;
        }
        let (active_open, open) = {
            let rows = self.list_rows(true).await;
            let active_open = rows.iter().any(|row| row["status"] == "opening");
            let open = rows
                .iter()
                .filter(|row| row["status"] == "open")
                .filter_map(|row| row["sessionId"].as_str().map(str::to_owned))
                .collect::<Vec<_>>();
            (active_open, open)
        };
        if !active_open {
            for handle in &open {
                if self
                    .handoff_closed
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .contains(handle)
                {
                    continue;
                }
                let busy = match &self.workers {
                    Some(workers) => workers
                        .worker(handle)
                        .is_some_and(|worker| worker.handoff_busy()),
                    None => {
                        let registry = self.registry.lock().await;
                        registry
                            .get(handle)
                            .and_then(|entry| entry.runtime.as_ref())
                            .is_some_and(|runtime| {
                                crate::handoff_activity::is_handoff_busy(
                                    runtime.session().activity_snapshot(),
                                )
                            })
                    }
                };
                if busy {
                    continue;
                }
                self.handoff_closed
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .insert(handle.clone());
                self.handoff_parks
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let core = std::sync::Arc::clone(self);
                let handle = handle.clone();
                tokio::spawn(async move {
                    core.park_session(&handle).await;
                    core.handoff_parks
                        .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                    if core.has_session(&handle).await {
                        core.handoff_closed
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .remove(&handle);
                    }
                });
            }
        }
        let size = self.session_count().await;
        let parks = self.handoff_parks.load(std::sync::atomic::Ordering::SeqCst);
        let step = {
            self.drain
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .step(active_open, size, parks)
        };
        matches!(step, crate::session_command_router::DrainStep::Exit)
    }
    /// The park teardown of `release_session` (senpi `tearDownReleased`): claimed in one synchronous step, every
    /// attachment drained, the runtime disposed and the path reservation freed before this resolves, and sealed
    /// `session_closed { reason: "released" }` naming the file. `false` when another close already owns the entry.
    async fn tear_down_released(&self, session_id: &str, session_path: &str) -> bool {
        if let Some(workers) = &self.workers {
            if workers.force_close(session_id).is_none() {
                return false;
            }
            if let Some(worker) = workers.worker(session_id) {
                worker.cancel_pending_extension_ui_requests();
            }
            let _ = workers.close_marked(session_id).await;
        } else {
            {
                let mut registry = self.registry.lock().await;
                let Some(entry) = registry.get_mut(session_id) else {
                    return false;
                };
                if entry.close.state != crate::session_registry::RpcSessionState::Open {
                    return false;
                }
                entry.close.attachments = 0;
                entry.close.state = crate::session_registry::RpcSessionState::Closing;
            }
            if let Some(binding) = self.bindings.lock().await.remove(session_id) {
                binding.cancel_pending_extension_ui_requests();
                binding.dispose().await;
            }
            let mut registry = self.registry.lock().await;
            let _ = registry
                .close_marked(session_id, self.options.close_grace_ms)
                .await;
        }
        if let Ok(value) =
            serde_json::to_value(crate::rpc_types::RpcSessionLifecycleEvent::SessionClosed {
                session_id: session_id.into(),
                reason: Some("released".into()),
                session_path: Some(session_path.to_owned()),
            })
        {
            self.route(session_id, None, value);
        }
        self.forget_session(session_id);
        true
    }
    /// The park teardown of `unload_session` (senpi `tearDownUnloaded`): claimed while still open, every attachment
    /// drained, the runtime disposed and the path reservation freed, sealed `session_parked { reason: "unloaded" }`.
    /// `false` when another close already owns the entry.
    async fn tear_down_unloaded(&self, session_id: &str, session_path: &str) -> bool {
        if let Some(workers) = &self.workers {
            if workers.force_close(session_id).is_none() {
                return false;
            }
            if let Some(worker) = workers.worker(session_id) {
                worker.cancel_pending_extension_ui_requests();
            }
            let _ = workers.close_marked(session_id).await;
        } else {
            {
                let mut registry = self.registry.lock().await;
                let Some(entry) = registry.get_mut(session_id) else {
                    return false;
                };
                if entry.close.state != crate::session_registry::RpcSessionState::Open {
                    return false;
                }
                entry.close.attachments = 0;
                entry.close.state = crate::session_registry::RpcSessionState::Closing;
            }
            if let Some(binding) = self.bindings.lock().await.remove(session_id) {
                binding.cancel_pending_extension_ui_requests();
                binding.dispose().await;
            }
            let mut registry = self.registry.lock().await;
            let _ = registry
                .close_marked(session_id, self.options.close_grace_ms)
                .await;
        }
        if let Ok(value) =
            serde_json::to_value(crate::rpc_types::RpcSessionLifecycleEvent::SessionParked {
                session_id: session_id.into(),
                session_path: session_path.to_owned(),
                reason: Some("unloaded".into()),
            })
        {
            self.route(session_id, None, value);
        }
        self.forget_session(session_id);
        true
    }
    /// Parks one settled session for a handoff (senpi `evictIdleSession(sessionId,"handoff_parked")`):
    /// the routing handle ends exactly as a close ends it, the terminal record names the reason (and
    /// the file a retained session reopens by) so a client is not told the session vanished, and
    /// every attachment is drained because a drain ends the shared session.
    async fn park_session(&self, session_id: &str) {
        if let Some(workers) = &self.workers {
            let record = workers
                .with_entry(session_id, |entry| {
                    self.park_record(
                        entry.close.retain_on_disconnect,
                        entry.session_path.as_deref(),
                        entry.durable_session_id.as_deref(),
                        &entry.profile,
                        entry.context_overlay.as_ref(),
                    )
                })
                .flatten();
            let Some(retained) = workers.force_close(session_id) else {
                return;
            };
            self.write_park_record(record.filter(|_| retained.is_some()))
                .await;
            let _ = workers.close_marked(session_id).await;
            if let Ok(value) =
                serde_json::to_value(crate::rpc_types::RpcSessionLifecycleEvent::SessionClosed {
                    session_id: session_id.into(),
                    reason: Some("handoff_parked".into()),
                    session_path: retained,
                })
            {
                self.route(session_id, None, value);
            }
            self.forget_session(session_id);
            return;
        }
        let (retained, record) = {
            let mut registry = self.registry.lock().await;
            let Some(entry) = registry.get_mut(session_id) else {
                return;
            };
            if entry.close.state != crate::session_registry::RpcSessionState::Open {
                return;
            }
            let retained = if entry.close.retain_on_disconnect {
                entry.session_path.clone()
            } else {
                None
            };
            let record = self.park_record(
                entry.close.retain_on_disconnect,
                entry.session_path.as_deref(),
                entry.durable_session_id.as_deref(),
                &entry.profile,
                entry.context_overlay.as_ref(),
            );
            entry.close.attachments = 0;
            entry.close.state = crate::session_registry::RpcSessionState::Closing;
            (retained, record)
        };
        // Written once the claim is won and awaited before the drain proceeds, or the generation can hand over
        // and exit before it lands and the park is lost (senpi `recordHandoverParkedSession`).
        self.write_park_record(record).await;
        if let Some(binding) = self.bindings.lock().await.remove(session_id) {
            binding.cancel_pending_extension_ui_requests();
            binding.dispose().await;
        }
        {
            let mut registry = self.registry.lock().await;
            let _ = registry
                .close_marked(session_id, self.options.close_grace_ms)
                .await;
        }
        if let Ok(value) =
            serde_json::to_value(crate::rpc_types::RpcSessionLifecycleEvent::SessionClosed {
                session_id: session_id.into(),
                reason: Some("handoff_parked".into()),
                session_path: retained,
            })
        {
            self.route(session_id, None, value);
        }
        self.forget_session(session_id);
    }
    /// Releases the RETAINED sessions a dropped connection still owned (senpi `releaseConnection`
    /// -> `releaseOwnedSession` for `retainOnDisconnect` entries): a retained session is never closed
    /// by a drop, so each attachment is released at once, and the release that leaves the session
    /// with no attachment parks it and tells its extensions `session_parked`.
    pub async fn release_retained_attachments(&self, connection: &str) {
        let owned = self
            .attachments
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .owned_by(connection);
        if let Some(workers) = &self.workers {
            for (session_id, count) in owned {
                for _ in 0..count {
                    let retained = workers
                        .with_entry(&session_id, |entry| {
                            entry.close.state == crate::session_registry::RpcSessionState::Open
                                && entry.close.retain_on_disconnect
                        })
                        .unwrap_or(false);
                    if !retained || workers.mark_close(&session_id, true).is_err() {
                        break;
                    }
                    self.attachments
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .release(connection, &session_id);
                }
            }
            return;
        }
        for (session_id, count) in owned {
            for _ in 0..count {
                let parked = {
                    let mut registry = self.registry.lock().await;
                    let Some(entry) = registry.get(&session_id) else {
                        break;
                    };
                    if entry.close.state != crate::session_registry::RpcSessionState::Open
                        || !entry.close.retain_on_disconnect
                    {
                        break;
                    }
                    let parks = entry.close.attachments == 1;
                    let parked = if parks { entry.runtime.clone() } else { None };
                    // A failed claim means the entry is already closed or owned by another path.
                    if registry.mark_close(&session_id, true).is_err() {
                        break;
                    }
                    parked
                };
                self.attachments
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .release(connection, &session_id);
                if let Some(runtime) = parked {
                    runtime
                        .emit_attachment_event(
                            maho_core::agent_session_runtime::AttachmentEvent::SessionParked,
                        )
                        .await;
                }
            }
        }
    }
    fn respond(&self, connection: Option<&str>, response: &crate::rpc_types::RpcResponse) {
        let Ok(value) = serde_json::to_value(response) else {
            return;
        };
        self.respond_record(
            connection,
            response.session_id.as_deref().unwrap_or(""),
            value,
        );
    }
    /// Answers an already-built response record (`release_session` builds its own wire answer), tagged to its session.
    fn respond_value(&self, connection: Option<&str>, session_id: &str, value: serde_json::Value) {
        self.respond_record(connection, session_id, value);
    }
    fn respond_record(
        &self,
        connection: Option<&str>,
        session_id: &str,
        mut value: serde_json::Value,
    ) {
        crate::lifecycle_delivery::observe_lifecycle_delivery(&mut value);
        if let Some(connection) = connection {
            match self
                .fanout
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .enqueue_control(connection, &value)
            {
                Ok(true) => return,
                Ok(false) => {}
                Err(error) => {
                    eprintln!("senpi rpc connection {connection} failed: {error}");
                    return;
                }
            }
        }
        let _ = self.default_writer.enqueue(session_id, value);
    }
    fn error(
        &self,
        connection: Option<&str>,
        id: Option<String>,
        command: &str,
        message: impl Into<String>,
        session_id: Option<String>,
    ) {
        self.respond(
            connection,
            &crate::rpc_types::RpcResponse {
                id,
                record_type: crate::rpc_types::ResponseRecordType::Response,
                command: command.into(),
                session_id,
                result: crate::rpc_types::RpcResponseResult::Error {
                    error: message.into(),
                    error_code: None,
                    error_data: None,
                },
            },
        );
    }
    /// A refusal whose stable code is also `errorCode`, with the `errorData` a client acts on (senpi router `error`).
    fn refuse(
        &self,
        connection: Option<&str>,
        id: Option<String>,
        command: &str,
        code: &str,
        data: serde_json::Value,
        session_id: Option<String>,
    ) {
        self.respond(
            connection,
            &crate::rpc_types::RpcResponse {
                id,
                record_type: crate::rpc_types::ResponseRecordType::Response,
                command: command.into(),
                session_id,
                result: crate::rpc_types::RpcResponseResult::Error {
                    error: code.into(),
                    error_code: Some(code.into()),
                    error_data: Some(data),
                },
            },
        );
    }
    fn success(
        &self,
        connection: Option<&str>,
        id: Option<String>,
        command: &str,
        data: Option<serde_json::Value>,
        session_id: Option<String>,
    ) {
        self.respond(
            connection,
            &crate::rpc_types::RpcResponse {
                id,
                record_type: crate::rpc_types::ResponseRecordType::Response,
                command: command.into(),
                session_id,
                result: crate::rpc_types::RpcResponseResult::Success { data },
            },
        );
    }
    fn write_presenter_reply(&self, connection: Option<&str>, reply: &serde_json::Value) {
        write_presenter_reply(
            &std::sync::Arc::downgrade(&self.fanout),
            &self.default_writer,
            &std::sync::Arc::downgrade(&self.presenters),
            connection,
            reply,
        );
    }
    /// Feed one inbound JSONL line on one connection (senpi's `handle` inside `createHostCore`).
    pub async fn handle(
        self: &std::sync::Arc<Self>,
        connection: Option<&str>,
        line: &str,
    ) -> Result<(), String> {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            // With presenters enabled a parse error never echoes the (possibly private) body.
            let message = if self.presenters.enabled() {
                "Invalid command".to_owned()
            } else {
                format!(
                    "Failed to parse command: {}",
                    crate::connection_handler::json_parse_error_message(line)
                )
            };
            self.error(connection, None, "parse", message, None);
            return Ok(());
        };
        // senpi `writer.handleMcpForm(parsed)`: private records bypass the gate, queues, snapshots and replay.
        if let Some(reply) = self.presenters.handle(&value, connection) {
            self.write_presenter_reply(connection, &reply);
            return Ok(());
        }
        // senpi `gate.intercept`: the conditional idle handover's own request, answered with `success: true`
        // whatever the verdict (a foreign agent dir is refused before the operation).
        if value["type"] == crate::host_handover_wire::BEGIN_HANDOVER_COMMAND
            && let Some(record) = value.as_object()
        {
            let gate = self.handover.get();
            let _answering = HandoverAnswering::new(self);
            let answer = crate::host_handover_wire::answer_begin_handover(
                gate.map(|gate| &gate.handover),
                gate.map(|gate| &gate.endpoint),
                record,
            )
            .await;
            self.respond_record(connection, "", answer);
            return Ok(());
        }
        if let Some(error) = crate::rpc_input_validation::rpc_command_shape_error(&value) {
            self.error(
                connection,
                value["id"].as_str().map(str::to_owned),
                "parse",
                error,
                None,
            );
            return Ok(());
        }
        let command = match serde_json::from_value::<crate::rpc_types::RpcCommand>(value.clone()) {
            Ok(command) => command,
            Err(error) => {
                let kind = value["type"].as_str().unwrap_or_default();
                let message = if error
                    .to_string()
                    .starts_with(&format!("unknown variant `{kind}`,"))
                {
                    format!("Unknown command: {kind}")
                } else {
                    error.to_string()
                };
                self.error(
                    connection,
                    value["id"].as_str().map(str::to_owned),
                    kind,
                    message,
                    None,
                );
                return Ok(());
            }
        };
        if let Some(error) = crate::rpc_input_validation::rpc_command_payload_error(&value)
            .map(str::to_owned)
            .or_else(|| crate::rpc_input_validation::rpc_message_length_error(&value))
        {
            self.error(
                connection,
                command.id.clone(),
                value["type"].as_str().unwrap_or_default(),
                error,
                command.session_id.clone(),
            );
            return Ok(());
        }
        // senpi router `handle`: socket ownership is checked before lifecycle routing and worker dispatch.
        let kind = value["type"].as_str().unwrap_or_default();
        let attached = command.session_id.as_deref().is_some_and(|session_id| {
            self.attachments
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .owns(connection, session_id)
        });
        match crate::session_attachment_gate::attachment_gate(
            connection,
            kind,
            command.session_id.as_deref(),
            attached,
            || std::env::vars().collect(),
        ) {
            crate::session_attachment_gate::AttachmentGate::Pass => {}
            crate::session_attachment_gate::AttachmentGate::Warn(line) => eprintln!("{line}"),
            crate::session_attachment_gate::AttachmentGate::Refuse => {
                self.refuse(
                    connection,
                    command.id.clone(),
                    kind,
                    crate::rpc_types::RPC_ERROR_SESSION_NOT_ATTACHED,
                    crate::session_attachment_gate::not_attached_detail(),
                    command.session_id.clone(),
                );
                return Ok(());
            }
        }
        // senpi `HostCoreGate.dispatch`: while a handover is pending, new work is refused; everything else is routed.
        if let Some(gate) = self.handover.get()
            && let Some(view) = gate.handover.view()
            && !gate.handover.admits(kind)
        {
            let refusal =
                crate::host_handover_wire::handover_refusal(command.id.as_deref(), kind, &view);
            self.respond_record(connection, "", refusal);
            return Ok(());
        }
        let _settled = HandoverObserve(self.handover.get().map(|gate| &gate.handover));
        let _sessionless = command.session_id.is_none().then(|| {
            self.sessionless_requests
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            SessionlessGuard(&self.sessionless_requests)
        });
        // senpi router `handle`: every routed request counts against its session until it answers, so a
        // release can tell other work from itself.
        let _request = self.begin_request(command.session_id.as_deref());
        match &command.body {
            crate::rpc_types::RpcCommandBody::AttachSession {
                durable_session_id,
                expected_host_instance,
            } => {
                let session_id = command.session_id.clone().unwrap_or_default();
                match self
                    .attach_session(
                        connection,
                        &session_id,
                        durable_session_id,
                        expected_host_instance,
                    )
                    .await
                {
                    Ok(()) => self.success(
                        connection,
                        command.id.clone(),
                        "attach_session",
                        Some(crate::session_attachment_commands::attach_reply(
                            &session_id,
                        )),
                        Some(session_id),
                    ),
                    Err((code, data)) => self.refuse(
                        connection,
                        command.id.clone(),
                        "attach_session",
                        code,
                        data,
                        Some(session_id),
                    ),
                }
            }
            crate::rpc_types::RpcCommandBody::ReleaseAttachment => {
                let session_id = command.session_id.clone().unwrap_or_default();
                self.release_attachment(connection, command.id.clone(), session_id)
                    .await;
            }
            crate::rpc_types::RpcCommandBody::ReclaimSession {
                session_path,
                durable_session_id,
                expected_host_instance,
            } => match self
                .reclaim_session(
                    connection,
                    session_path,
                    durable_session_id,
                    expected_host_instance,
                )
                .await
            {
                Ok(session_id) => self.success(
                    connection,
                    command.id.clone(),
                    "reclaim_session",
                    Some(crate::session_attachment_commands::reclaim_reply(
                        &session_id,
                    )),
                    None,
                ),
                Err((code, data)) => self.refuse(
                    connection,
                    command.id.clone(),
                    "reclaim_session",
                    code,
                    data,
                    None,
                ),
            },
            crate::rpc_types::RpcCommandBody::GetProtocolInfo => {
                // senpi `process.argv.slice(2)`: every CLI argument after the executable.
                let argv = std::env::args().skip(1).collect::<Vec<_>>();
                let mut data =
                    crate::protocol_identity::host_launch_profile(&argv, &self.options.cwd)
                        .ok()
                        .map_or_else(
                            || serde_json::json!({}),
                            |profile| {
                                // The agent dir this host captured at startup: only a client of it may
                                // replace this host (#3560).
                                crate::protocol_identity::protocol_identity_for(
                                    profile,
                                    &std::env::vars().collect(),
                                    &std::path::absolute(&self.options.agent_dir)
                                        .unwrap_or_else(|_| self.options.agent_dir.clone())
                                        .to_string_lossy(),
                                )
                            },
                        );
                data["protocolVersion"] = crate::host_decision::HOST_PROTOCOL_VERSION.into();
                data["serverVersion"] = maho_core::engine_build_identity::engine_build_identity()
                    .text
                    .clone()
                    .into();
                data["capabilities"] = serde_json::json!(self.advertised_capabilities());
                data["mode"] = "multi".into();
                let gate = self.handover.get();
                crate::host_handover_wire::with_host_identity(
                    &mut data,
                    self.runtime_build_id
                        .get()
                        .map(String::as_str)
                        .or_else(|| gate.and_then(|gate| gate.runtime_build_id.as_deref())),
                    gate.and_then(|gate| gate.handover.view()).as_ref(),
                );
                self.success(
                    connection,
                    command.id.clone(),
                    "get_protocol_info",
                    Some(data),
                    None,
                );
            }
            crate::rpc_types::RpcCommandBody::SetClientInfo {
                width,
                capabilities,
            } => {
                // Session-less: the connection's capabilities (senpi router `set_client_info`); with a
                // handle, also this connection's render width for that session (senpi `sharedWidth`).
                // senpi router `set_client_info`: a connection's declared role is fixed once declared.
                if command.session_id.is_none()
                    && let Some(capabilities) = capabilities
                    && !self
                        .connection_roles
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .declare(
                            crate::connection_roles::role_connection(connection),
                            capabilities,
                        )
                {
                    self.error(
                        connection,
                        command.id.clone(),
                        "set_client_info",
                        crate::connection_roles::CONNECTION_ROLE_FIXED,
                        None,
                    );
                    return Ok(());
                }
                if let Some(connection) = connection {
                    let capabilities = capabilities.clone().unwrap_or_default();
                    self.pending_capabilities
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .insert(connection.to_owned(), capabilities.clone());
                    if let Err(error) = self
                        .fanout
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .set_capabilities(connection, &capabilities)
                    {
                        eprintln!("senpi rpc connection {connection} failed: {error}");
                    }
                    // A connection that stops declaring `interactive_user` + `mcp_form_presenter_v1` loses its grants.
                    self.presenters
                        .capabilities_changed(connection, &capabilities);
                    if let Some(session_id) = &command.session_id {
                        self.widths
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .set_width(session_id, Some(connection), *width);
                    }
                }
                self.success(
                    connection,
                    command.id.clone(),
                    "set_client_info",
                    None,
                    command.session_id.clone(),
                );
            }
            // The port's sessions hold no pause/hold or prewarm state, so both acknowledge on a live session.
            crate::rpc_types::RpcCommandBody::ResumeSession
            | crate::rpc_types::RpcCommandBody::RecordUserActivity { .. } => {
                let kind = value["type"].as_str().unwrap_or_default().to_owned();
                let Some(session_id) = command.session_id.clone() else {
                    self.error(
                        connection,
                        command.id.clone(),
                        &kind,
                        crate::rpc_types::RPC_ERROR_MISSING_SESSION_ID,
                        None,
                    );
                    return Ok(());
                };
                if self.has_session(&session_id).await {
                    self.success(
                        connection,
                        command.id.clone(),
                        &kind,
                        None,
                        Some(session_id),
                    );
                } else {
                    self.error(
                        connection,
                        command.id.clone(),
                        &kind,
                        crate::rpc_types::RPC_ERROR_UNKNOWN_SESSION,
                        Some(session_id),
                    );
                }
            }
            // senpi router: every subscription account's latest limit windows, answered without a session.
            crate::rpc_types::RpcCommandBody::GetSubscriptionLimits => {
                let accounts = serde_json::to_value(
                    maho_core::subscription_limits::limit_store::subscription_limits().snapshot(),
                )
                .unwrap_or_else(|_| serde_json::json!([]));
                self.success(
                    connection,
                    command.id.clone(),
                    "get_subscription_limits",
                    Some(serde_json::json!({ "accounts": accounts })),
                    None,
                );
            }
            crate::rpc_types::RpcCommandBody::ListSessions { include_workers } => {
                let mut sessions = self.list_rows(include_workers.unwrap_or(false)).await;
                let kernels =
                    maho_core::memory_report::kernel_registry_read::read_codemode_kernel_rows();
                for row in &mut sessions {
                    let memory = session_memory(&kernels, row["sessionId"].as_str().unwrap_or(""));
                    row["memory"] = memory;
                }
                self.success(
                    connection,
                    command.id.clone(),
                    "list_sessions",
                    Some(serde_json::json!({"sessions":sessions})),
                    None,
                );
            }
            crate::rpc_types::RpcCommandBody::OpenSession { .. } => {
                // senpi router `handle`: a draining host parks every session itself and admits no open.
                if self.is_draining() {
                    self.error(
                        connection,
                        command.id.clone(),
                        "open_session",
                        crate::rpc_types::RPC_ERROR_HOST_DRAINING,
                        None,
                    );
                    return Ok(());
                }
                // Settles once the open answered (dropped at the end of this arm).
                let _barrier = self.open_barrier(connection, command.id.as_deref());
                match self
                    .open_session_inner(connection, &command.body, None)
                    .await
                {
                    Ok(data) => {
                        self.success(
                            connection,
                            command.id.clone(),
                            "open_session",
                            Some(data),
                            None,
                        );
                    }
                    Err(OpenRefusal {
                        code: Some(code),
                        data: Some(data),
                        ..
                    }) => {
                        self.refuse(
                            connection,
                            command.id.clone(),
                            "open_session",
                            &code,
                            data,
                            None,
                        );
                    }
                    Err(refusal) => {
                        self.error(
                            connection,
                            command.id.clone(),
                            "open_session",
                            refusal.message,
                            None,
                        );
                    }
                }
            }
            crate::rpc_types::RpcCommandBody::ReleaseSession {
                reason,
                move_id,
                target,
                interrupt,
                force,
            } => {
                let Some(session_id) = command.session_id.clone() else {
                    self.error(
                        connection,
                        command.id.clone(),
                        "release_session",
                        crate::rpc_types::RPC_ERROR_MISSING_SESSION_ID,
                        None,
                    );
                    return Ok(());
                };
                let release = crate::session_release::ReleaseSessionCommand {
                    id: command.id.clone(),
                    session_id: session_id.clone(),
                    reason: reason.clone(),
                    move_id: move_id.clone(),
                    target: target.clone(),
                    force: *force,
                    interrupt: *interrupt,
                };
                let port = HostReleasePort {
                    core: self,
                    connection,
                };
                let response = crate::session_release::release_session(&port, &release).await;
                self.respond_value(connection, &session_id, response);
            }
            crate::rpc_types::RpcCommandBody::UnloadSession { .. } => {
                let Some(session_id) = command.session_id.clone() else {
                    self.error(
                        connection,
                        command.id.clone(),
                        "unload_session",
                        crate::rpc_types::RPC_ERROR_MISSING_SESSION_ID,
                        None,
                    );
                    return Ok(());
                };
                let port = HostUnloadPort {
                    release: HostReleasePort {
                        core: self,
                        connection,
                    },
                };
                let response = crate::session_unload::unload_session(&port, &value).await;
                self.respond_value(connection, &session_id, response);
            }
            crate::rpc_types::RpcCommandBody::SubscribeSessions => {
                self.subscribe_sessions(connection, &value);
            }
            crate::rpc_types::RpcCommandBody::UnsubscribeSessions => {
                self.unsubscribe_sessions(connection, &value);
            }
            crate::rpc_types::RpcCommandBody::SetSessionContext { patch } => {
                let Some(session_id) = command.session_id.clone() else {
                    self.error(
                        connection,
                        command.id.clone(),
                        "set_session_context",
                        crate::rpc_types::RPC_ERROR_MISSING_SESSION_ID,
                        None,
                    );
                    return Ok(());
                };
                self.set_session_context(connection, command.id.clone(), session_id, patch)
                    .await;
            }
            crate::rpc_types::RpcCommandBody::CloseSession => {
                let Some(session_id) = command.session_id.clone().or(self.only_session().await)
                else {
                    self.error(
                        connection,
                        command.id.clone(),
                        "close_session",
                        crate::rpc_types::RPC_ERROR_UNKNOWN_SESSION,
                        None,
                    );
                    return Ok(());
                };
                if let Some(workers) = self.workers.clone() {
                    self.close_worker_session(&workers, connection, command.id.clone(), session_id)
                        .await;
                    return Ok(());
                }
                let admitted = {
                    let mut registry = self.registry.lock().await;
                    let mut attachments = self
                        .attachments
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    match registry.get_mut(&session_id) {
                        Some(entry) => attachments.admit_close(
                            (connection, &session_id),
                            &serde_json::json!({}),
                            &mut entry.close,
                            false,
                            &self.default_writer,
                        ),
                        None => Err(crate::rpc_types::RPC_ERROR_UNKNOWN_SESSION.to_owned()),
                    }
                };
                match admitted {
                    Err(error) => self.error(
                        connection,
                        command.id.clone(),
                        "close_session",
                        error,
                        Some(session_id),
                    ),
                    Ok(None) => {}
                    Ok(Some((reservation, claim))) => {
                        self.default_writer.release_close_response(reservation);
                        if let Some(connection) = connection {
                            self.release_owner_attachment(connection, &session_id).await;
                        }
                        // Only the LAST attachment tears the session down; a surviving attachment keeps
                        // the shared binding and its event stream (senpi `close`/`claimClose`).
                        if claim.finalizer != Some(true) {
                            // senpi `close`: a surviving attachment is a detach; the last one seals instead.
                            self.publish_status(&session_id, "detach", "client", false)
                                .await;
                            self.success(
                                connection,
                                command.id.clone(),
                                "close_session",
                                None,
                                Some(session_id),
                            );
                            return Ok(());
                        }
                        if let Some(binding) = self.bindings.lock().await.remove(&session_id) {
                            binding.dispose().await;
                        }
                        let completion = self
                            .registry
                            .lock()
                            .await
                            .close_marked(&session_id, self.options.close_grace_ms)
                            .await;
                        let result = match completion {
                            Err(error) => Err(error.to_string()),
                            Ok(Some(crate::session_teardown::RuntimeCloseCompletion::Settled(
                                result,
                            ))) => result
                                .map_err(|error| error.to_string())
                                .and_then(|result| result.map_err(|error| error.to_string())),
                            Ok(Some(
                                crate::session_teardown::RuntimeCloseCompletion::Draining(task),
                            )) => task
                                .await
                                .map_err(|error| error.to_string())
                                .and_then(|result| result.map_err(|error| error.to_string())),
                            Ok(None) => Ok(()),
                        };
                        // The terminal record precedes the close answer, and follows the lifecycle
                        // visibility rule (senpi `reply.complete(true,"client_close")`).
                        if let Ok(value) = serde_json::to_value(
                            crate::rpc_types::RpcSessionLifecycleEvent::SessionClosed {
                                session_id: session_id.clone(),
                                reason: Some("client_close".into()),
                                session_path: None,
                            },
                        ) {
                            self.route(&session_id, None, value);
                        }
                        self.forget_session(&session_id);
                        match result {
                            Ok(()) => self.success(
                                connection,
                                command.id.clone(),
                                "close_session",
                                None,
                                Some(session_id),
                            ),
                            Err(error) => self.error(
                                connection,
                                command.id.clone(),
                                "close_session",
                                error,
                                Some(session_id),
                            ),
                        }
                    }
                }
            }
            _ => {
                let Some(session_id) = command.session_id.clone() else {
                    self.error(
                        connection,
                        command.id.clone(),
                        value["type"].as_str().unwrap_or_default(),
                        crate::rpc_types::RPC_ERROR_MISSING_SESSION_ID,
                        None,
                    );
                    return Ok(());
                };
                // An open session whose output is sealed answers every command with the recovery it needs.
                if self.default_writer.is_session_sealed(&session_id)
                    && self.session_open(&session_id).await
                {
                    self.refuse(
                        connection,
                        command.id.clone(),
                        value["type"].as_str().unwrap_or_default(),
                        crate::rpc_types::RPC_ERROR_SESSION_OUTPUT_OVERFLOW,
                        serde_json::json!({"sessionId":session_id,"action":"unload_and_reopen"}),
                        Some(session_id),
                    );
                    return Ok(());
                }
                if let Some(workers) = &self.workers {
                    let kind = value["type"].as_str().unwrap_or_default().to_owned();
                    match workers.get_for_command(
                        &session_id,
                        &kind,
                        crate::connection_handler::now_ms(),
                    ) {
                        Err(error) => self.error(
                            connection,
                            command.id.clone(),
                            &kind,
                            error.to_string(),
                            Some(session_id),
                        ),
                        Ok(worker) => {
                            let result = crate::session_attribution::run_with_session_attribution(
                                &self.activity,
                                crate::session_attribution::SessionAttribution {
                                    session_id: Some(session_id.clone()),
                                    tool: None,
                                    job: None,
                                },
                                worker.command(
                                    value.clone(),
                                    connection.map(str::to_owned),
                                    self.declared_roles(connection),
                                ),
                            )
                            .await;
                            if let Err(error) = result {
                                self.error(
                                    connection,
                                    command.id.clone(),
                                    &kind,
                                    error,
                                    Some(session_id),
                                );
                            }
                        }
                    }
                    return Ok(());
                }
                // A routed command restarts the session's idle window (senpi `registry.getForCommand`).
                if let Some(entry) = self.registry.lock().await.get_mut(&session_id) {
                    entry.last_command_at = crate::connection_handler::now_ms();
                }
                let binding = self.bindings.lock().await.get(&session_id).cloned();
                // Every routed command runs inside its session's attribution scope (senpi router
                // `handle`) and its connection's scope, so a response reaches only its requester.
                match binding {
                    Some(binding) => {
                        crate::session_attribution::run_with_session_attribution(
                            &self.activity,
                            crate::session_attribution::SessionAttribution {
                                session_id: Some(session_id.clone()),
                                tool: None,
                                job: None,
                            },
                            CURRENT_CONNECTION
                                .scope(connection.map(str::to_owned), binding.handle(&value)),
                        )
                        .await?;
                        // senpi `createEntrySwitchSession` -> `syncRuntimeMetadata`: a replacement in place moves
                        // the entry's file, durable id and path claim with it.
                        if replaces_session(&value) {
                            self.registry.lock().await.sync_runtime_metadata();
                        }
                    }
                    None => self.error(
                        connection,
                        command.id.clone(),
                        value["type"].as_str().unwrap_or_default(),
                        crate::rpc_types::RPC_ERROR_UNKNOWN_SESSION,
                        Some(session_id),
                    ),
                }
            }
        }
        Ok(())
    }
    async fn session_state(&self, handle: &str) -> serde_json::Value {
        let registry = self.registry.lock().await;
        registry
            .get(handle)
            .and_then(|entry| entry.runtime.as_ref())
            .map_or_else(
                || serde_json::json!({}),
                |runtime| {
                    crate::connection_handler::build_rpc_session_state(runtime.session(), None)
                },
            )
    }
    async fn only_session(&self) -> Option<String> {
        if self.session_count().await == 1 {
            self.list_rows(true)
                .await
                .first()
                .and_then(|row| row["sessionId"].as_str().map(str::to_owned))
        } else {
            None
        }
    }
    async fn list_rows(&self, include_workers: bool) -> Vec<serde_json::Value> {
        match &self.workers {
            Some(workers) => workers.list_sessions(include_workers),
            None => self.registry.lock().await.list_sessions(include_workers),
        }
    }
    async fn has_session(&self, handle: &str) -> bool {
        match &self.workers {
            Some(workers) => workers.worker(handle).is_some(),
            None => self.registry.lock().await.get(handle).is_some(),
        }
    }
    /// The routes a worker's output and lifecycle take back into this host's fanout.
    fn worker_routes(
        &self,
        workers: &std::sync::Arc<crate::worker_session_registry::WorkerSessionRegistry>,
        handle: &str,
    ) -> crate::worker_session_registry::WorkerRoutes {
        let (fanout, writer) = (
            std::sync::Arc::clone(&self.fanout),
            std::sync::Arc::clone(&self.default_writer),
        );
        let (failure_fanout, failure_writer) = (
            std::sync::Arc::clone(&self.fanout),
            std::sync::Arc::clone(&self.default_writer),
        );
        let (terminal_fanout, terminal_writer) = (
            std::sync::Arc::clone(&self.fanout),
            std::sync::Arc::clone(&self.default_writer),
        );
        let (close_fanout, close_writer) = (
            std::sync::Arc::clone(&self.fanout),
            std::sync::Arc::clone(&self.default_writer),
        );
        let (tag, failure_tag, terminal_tag, close_tag) = (
            handle.to_owned(),
            handle.to_owned(),
            handle.to_owned(),
            handle.to_owned(),
        );
        let registry = std::sync::Arc::downgrade(workers);
        crate::worker_session_registry::WorkerRoutes {
            output: Box::new(move |record, owner| {
                route_session_record(&fanout, &writer, &tag, owner.as_deref(), record)
            }),
            failure: Box::new(move |error| {
                route_session_record(
                    &failure_fanout,
                    &failure_writer,
                    &failure_tag,
                    None,
                    serde_json::json!({"type":"session_error","error":error}),
                )
            }),
            terminal_failure: Box::new(move |error| {
                if let Ok(value) = serde_json::to_value(
                    crate::rpc_types::RpcSessionLifecycleEvent::SessionClosed {
                        session_id: terminal_tag.clone(),
                        reason: Some("error".into()),
                        session_path: None,
                    },
                ) {
                    route_session_record(
                        &terminal_fanout,
                        &terminal_writer,
                        &terminal_tag,
                        None,
                        value,
                    );
                }
                eprintln!("senpi rpc session {terminal_tag} closed: {error}");
                terminal_fanout
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .forget_session(&terminal_tag);
                terminal_writer.forget_session(&terminal_tag);
            }),
            // senpi `requestClose` = `router.close({ type: "close_session" })` for the worker's own shutdown.
            request_close: Box::new(move || {
                let Some(registry) = registry.upgrade() else {
                    return;
                };
                if registry.force_close(&close_tag).is_none() {
                    return;
                }
                let (fanout, writer, tag) = (
                    std::sync::Arc::clone(&close_fanout),
                    std::sync::Arc::clone(&close_writer),
                    close_tag.clone(),
                );
                tokio::spawn(async move {
                    let _ = registry.close_marked(&tag).await;
                    if let Ok(value) = serde_json::to_value(
                        crate::rpc_types::RpcSessionLifecycleEvent::SessionClosed {
                            session_id: tag.clone(),
                            reason: Some("client_close".into()),
                            session_path: None,
                        },
                    ) {
                        route_session_record(&fanout, &writer, &tag, None, value);
                    }
                    fanout
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .forget_session(&tag);
                    writer.forget_session(&tag);
                });
            }),
            // senpi `SessionEventWriter`'s grant methods: this host's fanout is the connection view, and the settled
            // dialogs and notice snapshots belong to the security presentations the writer holds.
            security: Some(std::sync::Arc::new(HostSecurityWriter {
                fanout: std::sync::Arc::clone(&self.fanout),
            })
                as std::sync::Arc<
                    dyn crate::session_worker_client::WorkerSecurityWriter,
                >),
            presenters: self
                .presenters
                .enabled()
                .then(|| std::sync::Arc::clone(&self.presenters)),
        }
    }
    /// `open_session` on the worker runtime (senpi `WorkerSessionRegistry.openSession` + the router's
    /// `open`): a new handle spawns and binds its own worker; a known path attaches to the live one.
    /// One open, shared by `open_session` and the successor's parked reopen (senpi `open`): validated, admitted,
    /// attached, and answered as the data of the success response.
    async fn open_session_inner(
        &self,
        connection: Option<&str>,
        body: &crate::rpc_types::RpcCommandBody,
        overlay: Option<&serde_json::Value>,
    ) -> Result<serde_json::Value, OpenRefusal> {
        let crate::rpc_types::RpcCommandBody::OpenSession {
            session_path,
            cwd,
            provider,
            model_id,
            thinking_level,
            permission_preset,
            retain_on_disconnect,
            kind,
            context,
            auto_title,
            durable_session_id,
            attended,
            prompt_surface,
            browser_engine,
            retry_fallback,
            spawned_by,
        } = body
        else {
            return Err(OpenRefusal::plain(
                crate::rpc_types::RPC_ERROR_INVALID_LAUNCH_PROFILE,
            ));
        };
        let invalid = |reason: String| {
            OpenRefusal::plain(format!(
                "{}: {reason}",
                crate::rpc_types::RPC_ERROR_INVALID_LAUNCH_PROFILE
            ))
        };
        if crate::session_router_open_profile::invalid_spawned_by(spawned_by) {
            return Err(OpenRefusal::plain(
                crate::rpc_types::RPC_ERROR_INVALID_SPAWNED_BY,
            ));
        }
        // senpi session-command-router `open`: only `attended:false` (observational open) is accepted.
        if *attended == Some(true) {
            return Err(invalid("attended must be false or omitted".to_owned()));
        }
        if let Some(reason) =
            crate::rpc_input_validation::prompt_surface_error(prompt_surface.as_ref())
        {
            return Err(invalid(reason.to_owned()));
        }
        if let Some(reason) =
            crate::rpc_input_validation::browser_engine_error(browser_engine.as_ref())
        {
            return Err(invalid(reason.to_owned()));
        }
        if let Some(reason) =
            crate::rpc_input_validation::retry_fallback_error(retry_fallback.as_ref())
        {
            return Err(invalid(reason));
        }
        let profile = crate::session_registry::RpcSessionLaunchProfile {
            runtime: maho_core::agent_session_runtime::AgentSessionLaunchProfile {
                cwd: cwd.clone().unwrap_or_else(|| self.options.cwd.clone()),
                permission_preset: permission_preset.clone(),
                creation_model: provider.clone().zip(model_id.clone()),
                initial_thinking_level: thinking_level.clone(),
                auto_title: *auto_title,
                prompt_size_inventory: None,
            },
            session_path: session_path.clone(),
            durable_session_id: durable_session_id.clone(),
            session_kind: kind.map(|kind| match kind {
                crate::rpc_types::SessionKind::Worker => maho_ext_api::SessionKind::Worker,
                crate::rpc_types::SessionKind::Interactive => {
                    maho_ext_api::SessionKind::Interactive
                }
            }),
            session_context: context.clone(),
            attended: (*attended == Some(false)).then_some(false),
            prompt_surface: prompt_surface
                .as_ref()
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned),
            browser_engine: browser_engine
                .as_ref()
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned),
            retry_fallback: retry_fallback.clone(),
            holder_endpoint: Some(maho_core::session_holders::SessionHolderEndpoint {
                host_instance: Some(crate::protocol_identity::host_instance_id().to_owned()),
                host_socket: None,
                host_daemon_dir: std::env::var(crate::host_daemon_paths::HOST_DAEMON_DIR_ENV)
                    .ok()
                    .filter(|dir| !dir.is_empty()),
            }),
            // senpi `open`: `{ ...profile, spawnedBy: ancestry }` once validated.
            spawned_by: serde_json::from_value(spawned_by.clone()).ok(),
            session_start_reason: None,
        };
        let retain = retain_on_disconnect.unwrap_or(false);
        let opened = self
            .open_admitted(connection, profile, retain, overlay)
            .await;
        // senpi `open`: a successful open of a path drops any park record for it, so a later successor does not
        // reopen a session a client already took back.
        if opened.is_ok()
            && let (Some(path), Some(dir)) = (session_path, self.handover_parked_dir.get())
        {
            crate::host_handover_parked::clear_handover_parked_session(dir, path).await;
        }
        opened
    }
    async fn open_admitted(
        &self,
        connection: Option<&str>,
        profile: crate::session_registry::RpcSessionLaunchProfile,
        retain: bool,
        overlay: Option<&serde_json::Value>,
    ) -> Result<serde_json::Value, OpenRefusal> {
        if let Some(workers) = self.workers.clone() {
            return self
                .open_worker_session(&workers, connection, profile, retain, overlay)
                .await;
        }
        // senpi `attachToOpenSession`: a path still opening is waited on (bounded, this waiter only) with no
        // registry lock held, then admitted again so the reopen attaches to the session it opened.
        let admitted = loop {
            let admitted = {
                let mut registry = self.registry.lock().await;
                registry.admit_open(profile.clone(), retain, crate::connection_handler::now_ms())
            };
            match admitted {
                Ok(crate::session_registry::OpenAdmission::Wait {
                    completion,
                    session_path,
                }) => {
                    if let Err(error) = crate::session_registry_attach::wait_open_completion(
                        completion,
                        &session_path,
                    )
                    .await
                    {
                        break Err(error);
                    }
                }
                other => break other,
            }
        };
        match admitted {
            Err(error) => Err(OpenRefusal::from_registry(&error)),
            Ok(crate::session_registry::OpenAdmission::Wait { .. }) => Err(OpenRefusal::plain(
                crate::rpc_types::RPC_ERROR_UNKNOWN_SESSION,
            )),
            Ok(crate::session_registry::OpenAdmission::Attached(handle))
                if self.default_writer.is_session_sealed(&handle) =>
            {
                // A reattach to a session whose output is already sealed undoes only this attachment and keeps the
                // seal intact until an explicit unload parks it for a fresh open by path (senpi#3913).
                let session_path = {
                    let mut registry = self.registry.lock().await;
                    let path = registry
                        .get(&handle)
                        .and_then(|entry| entry.session_path.clone());
                    let _ = registry.mark_close(&handle, true);
                    path
                };
                let mut data = serde_json::json!({"sessionId":handle,"action":"unload_and_reopen"});
                if let Some(path) = session_path {
                    data["sessionPath"] = path.into();
                }
                Err(OpenRefusal::sealed(data))
            }
            Ok(crate::session_registry::OpenAdmission::Attached(handle)) => {
                // senpi session-registry.ts: an attach that ends a retained session's parked
                // (zero-attachment) window tells its extensions `session_resumed`.
                let resumed = {
                    let registry = self.registry.lock().await;
                    registry
                        .get(&handle)
                        .filter(|entry| {
                            entry.close.retain_on_disconnect && entry.close.attachments == 1
                        })
                        .and_then(|entry| entry.runtime.clone())
                };
                if let Some(runtime) = resumed {
                    runtime
                        .emit_attachment_event(
                            maho_core::agent_session_runtime::AttachmentEvent::SessionResumed,
                        )
                        .await;
                }
                // A second client joins the live session: register it with the fanout so it
                // receives the session's records and the in-flight snapshot (senpi `open`).
                self.attach_connection(connection, &handle).await;
                let state = self.session_state(&handle).await;
                self.publish_status(&handle, "attach", "client", false)
                    .await;
                Ok(serde_json::json!({"sessionId":handle,"state":state,"attached":true}))
            }
            Ok(crate::session_registry::OpenAdmission::Create(handle)) => {
                let created = {
                    let mut registry = self.registry.lock().await;
                    // senpi `runtimeProfile`: a resume drops the create-only model and thinking level and starts
                    // with `session_start { reason: "resume" }`.
                    let runtime_profile = registry.take_runtime_profile(&handle).unwrap_or(profile);
                    registry
                        .create_admitted_runtime(
                            &handle,
                            (self.options.create_runtime)(runtime_profile),
                        )
                        .await
                };
                if let Err(error) = created {
                    return Err(OpenRefusal::from_registry(&error));
                }
                if let Some(overlay) = overlay.and_then(serde_json::Value::as_object) {
                    let overlay = overlay.clone();
                    if let Some(entry) = self.registry.lock().await.get_mut(&handle) {
                        entry.context_overlay = Some(overlay);
                    }
                }
                let state = self.session_state(&handle).await;
                self.attach_connection(connection, &handle).await;
                self.publish_status(&handle, "open", "client", false).await;
                Ok(serde_json::json!({"sessionId":handle,"state":state}))
            }
        }
    }
    /// `set_session_context` (senpi `setSessionContext` + `answerSetSessionContext`): any attached connection
    /// may patch; a host with no per-connection identity (stdio) owns every session.
    async fn set_session_context(
        &self,
        connection: Option<&str>,
        id: Option<String>,
        session_id: String,
        patch: &serde_json::Value,
    ) {
        let refusal = |code: &str, reason: &str, action: &str| {
            let mut record = serde_json::json!({
                "type": "response",
                "command": "set_session_context",
                "success": false,
                "error": format!("{code}: {reason}"),
                "errorCode": code,
                "errorData": {"reason": reason, "action": action},
            });
            if let Some(id) = &id {
                record["id"] = id.clone().into();
            }
            self.respond_value(connection, &session_id, record);
        };
        if !self
            .attachments
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .owns(connection, &session_id)
        {
            refusal(
                crate::rpc_types::RPC_ERROR_SESSION_NOT_ATTACHED,
                "this connection is not attached to that session",
                "attach first with open_session { sessionPath }, then patch its context",
            );
            return;
        }
        let now = crate::connection_handler::now_ms();
        let published = if let Some(workers) = &self.workers {
            match workers.get_for_command(&session_id, "set_session_context", now) {
                Err(error) => {
                    self.error(
                        connection,
                        id,
                        "set_session_context",
                        error.to_string(),
                        Some(session_id),
                    );
                    return;
                }
                Ok(_) => workers
                    .with_entry(&session_id, |entry| {
                        crate::session_context_patch::apply_context_patch(
                            entry.context_overlay.as_ref(),
                            patch,
                        )
                        .map(|overlay| {
                            entry.context_overlay = Some(overlay);
                            crate::session_context_patch::published_session_context(
                                &entry.identity.context,
                                entry.context_overlay.as_ref(),
                            )
                        })
                    })
                    .unwrap_or_else(|| {
                        Err(crate::session_context_patch::ContextPatchRefusal {
                            reason: crate::rpc_types::RPC_ERROR_UNKNOWN_SESSION.to_owned(),
                            action: String::new(),
                        })
                    }),
            }
        } else {
            let mut registry = self.registry.lock().await;
            let routed = registry
                .get_for_command(&session_id, "set_session_context", now)
                .map_err(|error| error.to_string());
            match routed {
                Err(message) => {
                    drop(registry);
                    self.error(
                        connection,
                        id,
                        "set_session_context",
                        message,
                        Some(session_id),
                    );
                    return;
                }
                Ok(entry) => crate::session_context_patch::apply_context_patch(
                    entry.context_overlay.as_ref(),
                    patch,
                )
                .map(|overlay| {
                    entry.context_overlay = Some(overlay);
                    crate::session_context_patch::published_session_context(
                        &entry.identity.context,
                        entry.context_overlay.as_ref(),
                    )
                }),
            }
        };
        match published {
            Err(refused) => refusal(
                crate::rpc_types::RPC_ERROR_INVALID_SESSION_CONTEXT,
                &refused.reason,
                &refused.action,
            ),
            Ok(context) => {
                self.publish_status(&session_id, "context", "client", true)
                    .await;
                self.success(
                    connection,
                    id,
                    "set_session_context",
                    Some(serde_json::json!({"context": context})),
                    Some(session_id),
                )
            }
        }
    }
    async fn open_worker_session(
        &self,
        workers: &std::sync::Arc<crate::worker_session_registry::WorkerSessionRegistry>,
        connection: Option<&str>,
        profile: crate::session_registry::RpcSessionLaunchProfile,
        retain: bool,
        overlay: Option<&serde_json::Value>,
    ) -> Result<serde_json::Value, OpenRefusal> {
        let requested = profile.clone();
        let opened = workers
            .open_session(
                profile,
                retain,
                crate::connection_handler::now_ms(),
                |handle| self.worker_routes(workers, handle),
            )
            .await;
        let (handle, attached) = match opened {
            Err(error) => return Err(OpenRefusal::from_registry(&error)),
            Ok(crate::worker_session_registry::WorkerOpen::Created(handle)) => (handle, false),
            Ok(crate::worker_session_registry::WorkerOpen::Attached(handle))
                if self.default_writer.is_session_sealed(&handle) =>
            {
                // senpi `open`: lost output cannot be replayed, so a reattach to a sealed session undoes only this
                // attachment and keeps the seal until an explicit unload parks it (senpi#3913).
                let session_path = workers
                    .with_entry(&handle, |entry| entry.session_path.clone())
                    .flatten();
                let _ = workers.mark_close(&handle, true);
                let mut data = serde_json::json!({"sessionId":handle,"action":"unload_and_reopen"});
                if let Some(path) = session_path {
                    data["sessionPath"] = path.into();
                }
                return Err(OpenRefusal::sealed(data));
            }
            Ok(crate::worker_session_registry::WorkerOpen::Attached(handle)) => {
                // senpi worker `attach`: the surface, browser engine and permission preset follow the client.
                if let Err(error) = workers.apply_attach_profile(&handle, &requested).await {
                    eprintln!("senpi rpc session {handle} attach profile failed: {error}");
                }
                (handle, true)
            }
        };
        if !attached && let Some(overlay) = overlay.and_then(serde_json::Value::as_object) {
            let overlay = overlay.clone();
            workers.with_entry(&handle, |entry| entry.context_overlay = Some(overlay));
        }
        let Some(worker) = workers.worker(&handle) else {
            return Err(OpenRefusal::plain(
                crate::rpc_types::RPC_ERROR_UNKNOWN_SESSION,
            ));
        };
        let kind = workers
            .with_entry(&handle, |entry| entry.identity.kind)
            .unwrap_or_default();
        let capabilities = connection
            .and_then(|connection| {
                self.pending_capabilities
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .get(connection)
                    .cloned()
            })
            .unwrap_or_else(|| self.options.capabilities.clone());
        let mut declared = false;
        {
            let mut fanout = self
                .fanout
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            fanout.set_session_kind(&handle, kind);
            if let Some(connection) = connection {
                if fanout.targets.capabilities(connection).is_none() {
                    if let Err(error) = fanout.set_capabilities(connection, &capabilities) {
                        eprintln!("senpi rpc connection {connection} failed: {error}");
                    }
                    declared = true;
                }
                if let Err(error) =
                    fanout.attach(connection, &handle, crate::connection_handler::now_ms())
                {
                    eprintln!("senpi rpc connection {connection} failed: {error}");
                }
            }
        }
        if let Some(connection) = connection {
            // senpi `setConnectionCapabilities` always withdraws a non-presenting connection's grants.
            if declared {
                self.presenters
                    .capabilities_changed(connection, &capabilities);
            }
            self.attachments
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .attach(connection, &handle);
            self.presenters.attach(connection, &handle);
        }
        worker.set_connection_roles(std::sync::Arc::clone(&self.connection_roles));
        // senpi `SessionWorkerClient.bind`: the host's lane for this worker registers with the writer.
        if !attached && let Some(lane) = worker.acceptance() {
            self.default_writer
                .register_acceptance(&self.fanout, &handle, lane);
        }
        if !attached
            && let Err(error) = worker
                .bind(
                    &handle,
                    capabilities,
                    connection.map(str::to_owned),
                    self.declared_roles(connection),
                )
                .await
        {
            if let Some(connection) = connection {
                self.release_owner_attachment(connection, &handle).await;
            }
            if workers.force_close(&handle).is_some() {
                let _ = workers.close_marked(&handle).await;
            }
            self.forget_session(&handle);
            return Err(OpenRefusal::plain(format!("open_failed: {error}")));
        }
        let state = worker
            .snapshot()
            .map_or_else(|| serde_json::json!({}), |snapshot| snapshot.state);
        let mut data = serde_json::json!({"sessionId":handle,"state":state});
        if attached {
            data["attached"] = true.into();
        }
        self.publish_status(
            &handle,
            if attached { "attach" } else { "open" },
            "client",
            false,
        )
        .await;
        Ok(data)
    }
    /// `close_session` on the worker runtime: only the LAST attachment ends the worker, whose actual
    /// exit releases its reservations (senpi `WorkerSessionRegistry.beginClose` + `closeMarked`).
    async fn close_worker_session(
        &self,
        workers: &std::sync::Arc<crate::worker_session_registry::WorkerSessionRegistry>,
        connection: Option<&str>,
        id: Option<String>,
        session_id: String,
    ) {
        if !self
            .attachments
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .owns(connection, &session_id)
        {
            self.error(
                connection,
                id,
                "close_session",
                crate::rpc_types::RPC_ERROR_UNKNOWN_SESSION,
                Some(session_id),
            );
            return;
        }
        let claim = match workers.mark_close(&session_id, false) {
            Ok(claim) => claim,
            Err(error) => {
                self.error(
                    connection,
                    id,
                    "close_session",
                    error.to_string(),
                    Some(session_id),
                );
                return;
            }
        };
        if let Some(connection) = connection {
            self.attachments
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .release(connection, &session_id);
            self.release_owner_attachment(connection, &session_id).await;
        }
        if claim.finalizer != Some(true) {
            self.publish_status(&session_id, "detach", "client", false)
                .await;
            self.success(connection, id, "close_session", None, Some(session_id));
            return;
        }
        if let Some(worker) = workers.worker(&session_id) {
            worker.dispose();
        }
        let result = workers.close_marked(&session_id).await;
        if let Ok(value) =
            serde_json::to_value(crate::rpc_types::RpcSessionLifecycleEvent::SessionClosed {
                session_id: session_id.clone(),
                reason: Some("client_close".into()),
                session_path: None,
            })
        {
            self.route(&session_id, None, value);
        }
        self.forget_session(&session_id);
        match result {
            Ok(()) => self.success(connection, id, "close_session", None, Some(session_id)),
            Err(error) => self.error(
                connection,
                id,
                "close_session",
                error.to_string(),
                Some(session_id),
            ),
        }
    }
    /// Attaches one opening connection to a session (senpi `open`): ownership, the fanout target
    /// with its capabilities and late-joiner replay, the session's visibility class, and the shared
    /// binding the first opener creates. A stdio open owns no connection and keeps no binding.
    async fn attach_connection(&self, connection: Option<&str>, session_id: &str) {
        let (runtime, kind, lane, scope) = {
            let registry = self.registry.lock().await;
            match registry.get(session_id) {
                Some(entry) => (
                    entry.runtime.as_ref().map(std::sync::Arc::clone),
                    entry.identity.kind,
                    entry.acceptance.clone(),
                    entry.scope.clone(),
                ),
                None => return,
            }
        };
        let capabilities = connection
            .and_then(|connection| {
                self.pending_capabilities
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .get(connection)
                    .cloned()
            })
            .unwrap_or_else(|| self.options.capabilities.clone());
        let mut declared = false;
        {
            let mut fanout = self
                .fanout
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            fanout.set_session_kind(session_id, kind);
            if let Some(connection) = connection {
                if fanout.targets.capabilities(connection).is_none() {
                    if let Err(error) = fanout.set_capabilities(connection, &capabilities) {
                        eprintln!("senpi rpc connection {connection} failed: {error}");
                    }
                    declared = true;
                }
                if let Err(error) =
                    fanout.attach(connection, session_id, crate::connection_handler::now_ms())
                {
                    eprintln!("senpi rpc connection {connection} failed: {error}");
                }
            }
        }
        let Some(connection) = connection else {
            return;
        };
        // senpi `setConnectionCapabilities` always withdraws a non-presenting connection's grants.
        if declared {
            self.presenters
                .capabilities_changed(connection, &capabilities);
        }
        self.attachments
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .attach(connection, session_id);
        self.presenters.attach(connection, session_id);
        let Some(runtime) = runtime else {
            return;
        };
        let mut bindings = self.bindings.lock().await;
        if bindings.contains_key(session_id) {
            return;
        }
        let fanout = std::sync::Arc::clone(&self.fanout);
        let writer = std::sync::Arc::clone(&self.default_writer);
        let tag = session_id.to_owned();
        // senpi `createRpcSessionBinding`: the lane registers with the writer and sees every record first.
        let acceptance = lane.map(|lane| {
            self.default_writer.register_acceptance(
                &self.fanout,
                session_id,
                std::sync::Arc::clone(&lane),
            );
            crate::session_binding::BindingAcceptance::new(lane, runtime.session().clone())
        });
        let sink: std::sync::Arc<dyn Fn(serde_json::Value) + Send + Sync> =
            std::sync::Arc::new(move |value: serde_json::Value| {
                if let Some(acceptance) = &acceptance {
                    acceptance.accept(&value);
                }
                let owner = CURRENT_CONNECTION.try_with(Clone::clone).ok().flatten();
                route_session_record(&fanout, &writer, &tag, owner.as_deref(), value);
            });
        let structured_scope = scope.clone();
        let sink =
            crate::session_binding::closed_scope_record_sink(session_id.to_owned(), scope, sink);
        let binding = std::sync::Arc::new(
            crate::session_binding::create_rpc_session_binding(
                session_id.to_owned(),
                runtime.session().clone(),
                sink,
                capabilities,
                Some(std::sync::Arc::clone(&self.connection_roles)),
                crate::host_session_control::profile_host_socket(&runtime)
                    .or_else(|| self.public_socket.get().cloned()),
            )
            .await,
        );
        // senpi session-binding.ts: `writeStructured` only when `structuredLaneEnabled()`.
        if crate::structured_session_records::structured_lane_enabled() {
            binding.install_structured_lane(
                runtime.session(),
                self.default_writer.structured_lanes.clone(),
                structured_scope,
            );
        }
        // senpi session-binding `bindMcpForms(session, ui)`, only when the deployment enforces presenters.
        if self.presenters.enabled() {
            let attached = self
                .fanout
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .targets
                .attached_connections(session_id);
            let (fanout, writer, presenters) = (
                std::sync::Arc::downgrade(&self.fanout),
                std::sync::Arc::clone(&self.default_writer),
                std::sync::Arc::downgrade(&self.presenters),
            );
            binding.bind_mcp_forms(
                &self.presenters,
                runtime.session(),
                &attached,
                std::sync::Arc::new(move |connection, reply| {
                    write_presenter_reply(&fanout, &writer, &presenters, connection, reply)
                }),
            );
        }
        bindings.insert(session_id.to_owned(), binding);
    }
    /// Releases every session a dropped connection still owned (senpi `releaseConnection`). Each
    /// attachment goes through the refcounted detach a close uses, so a session another connection
    /// still holds keeps running, a retained one stays open at zero attachments, and only the last
    /// owner tears it down. A turn in flight runs to settlement first; a drain owns teardown itself.
    pub async fn release_connection(self: &std::sync::Arc<Self>, connection: &str) {
        self.release_connection_attachments(connection).await;
        // senpi `releaseConnection`: the release settles only after the connection's opens did.
        self.opens_settled(connection).await;
    }
    async fn release_connection_attachments(self: &std::sync::Arc<Self>, connection: &str) {
        let owned = self
            .attachments
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take(connection);
        for session_id in owned.keys() {
            self.connection_left(session_id, connection).await;
        }
        if self
            .drain
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_draining()
        {
            return;
        }
        if let Some(workers) = self.workers.clone() {
            for (session_id, count) in owned {
                for _ in 0..count {
                    let retained = workers
                        .with_entry(&session_id, |entry| entry.close.retain_on_disconnect)
                        .unwrap_or(false);
                    // A turn in flight runs to settlement first (the worker reports `settled` snapshots).
                    if !retained
                        && let Some(worker) =
                            workers.worker(&session_id).filter(|worker| worker.busy())
                    {
                        self.release_worker_after_settle(worker, session_id.clone());
                        continue;
                    }
                    self.release_owned_session(&session_id).await;
                }
            }
            return;
        }
        for (session_id, count) in owned {
            for _ in 0..count {
                let (retained, session) = {
                    let registry = self.registry.lock().await;
                    match registry.get(&session_id) {
                        Some(entry) => (
                            entry.close.retain_on_disconnect,
                            entry
                                .runtime
                                .as_ref()
                                .filter(|_| {
                                    entry.close.state
                                        == crate::session_registry::RpcSessionState::Open
                                })
                                .map(|runtime| runtime.session().clone()),
                        ),
                        None => (false, None),
                    }
                };
                if !retained
                    && let Some(session) =
                        session.filter(maho_core::agent_session::AgentSession::is_streaming)
                {
                    self.release_after_settle(session, session_id.clone());
                    continue;
                }
                self.release_owned_session(&session_id).await;
            }
        }
    }
    /// senpi `openWithBarrier`: registers the open under its connection and, for an open with an id, tells the
    /// connection where it was queued before the open runs - every open accepted anywhere on this host counts,
    /// since one runtime serves them all (senpi#1844). A stdio open has no connection and no barrier.
    fn open_barrier(
        &self,
        connection: Option<&str>,
        request_id: Option<&str>,
    ) -> Option<OpenBarrier<'_>> {
        let owner = connection?;
        let (settle, settled) = tokio::sync::watch::channel(false);
        let in_flight = {
            let mut opens = self
                .opens_by_connection
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let in_flight = opens.values().map(Vec::len).sum::<usize>();
            opens.entry(owner.to_owned()).or_default().push(settled);
            in_flight
        };
        if let Some(request_id) = request_id {
            let queued = crate::rpc_types::RpcOpenQueuedEvent {
                record_type: crate::rpc_types::OpenQueuedType::Queued,
                for_request: request_id.to_owned(),
                position: (in_flight + 1) as f64,
                in_flight: in_flight as f64,
            };
            if let Ok(value) = serde_json::to_value(&queued)
                && let Err(error) = self
                    .fanout
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .enqueue_control(owner, &value)
            {
                eprintln!("senpi rpc connection {owner} failed: {error}");
            }
        }
        Some(OpenBarrier {
            host: self,
            owner: owner.to_owned(),
            settle,
        })
    }
    /// Resolves once every open `connection` had in flight has answered.
    async fn opens_settled(&self, connection: &str) {
        let pending = self
            .opens_by_connection
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(connection)
            .cloned()
            .unwrap_or_default();
        for mut settled in pending {
            let _ = settled.wait_for(|done| *done).await;
        }
    }
    /// senpi `releaseDisconnectAttachment`'s deferral: subscribes to the turn's settlement and releases the handle
    /// from a detached task, so neither a dropped connection nor a cancelled command future can skip the release.
    fn release_after_settle(
        self: &std::sync::Arc<Self>,
        session: maho_core::agent_session::AgentSession,
        session_id: String,
    ) {
        let (settled_tx, settled_rx) = tokio::sync::oneshot::channel::<()>();
        let settled = std::sync::Arc::new(std::sync::Mutex::new(Some(settled_tx)));
        let notify = std::sync::Arc::clone(&settled);
        let subscription = session.subscribe(std::sync::Arc::new(
            move |event: &maho_ext_api::AgentSessionEvent| {
                if matches!(
                    event,
                    maho_ext_api::AgentSessionEvent::AgentSettled
                        | maho_ext_api::AgentSessionEvent::AgentIdle
                ) && let Some(sender) = notify
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .take()
                {
                    let _ = sender.send(());
                }
            },
        ));
        // The turn may have settled between the check and the subscription.
        if !session.is_streaming()
            && let Some(sender) = settled
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take()
        {
            let _ = sender.send(());
        }
        let core = std::sync::Arc::clone(self);
        tokio::spawn(async move {
            let _ = settled_rx.await;
            drop(subscription);
            core.release_owned_session(&session_id).await;
        });
    }
    /// The worker form of [`Self::release_after_settle`] (senpi `worker.subscribeSettled`).
    fn release_worker_after_settle(
        self: &std::sync::Arc<Self>,
        worker: std::sync::Arc<crate::session_worker_client::SessionWorkerClient>,
        session_id: String,
    ) {
        let mut settled = worker.settled();
        let core = std::sync::Arc::clone(self);
        tokio::spawn(async move {
            while worker.busy() && !worker.is_stopped() {
                if settled.changed().await.is_err() {
                    break;
                }
            }
            core.release_owned_session(&session_id).await;
        });
    }
    /// One owned handle's refcounted detach (senpi `releaseOwnedSession`): tolerant of a lost race,
    /// and only the LAST attachment disposes the binding and closes the runtime.
    async fn release_owned_session(&self, session_id: &str) {
        if let Some(workers) = &self.workers {
            let Ok(claim) = workers.mark_close(session_id, true) else {
                return;
            };
            if claim.finalizer != Some(true) {
                self.publish_status(session_id, "detach", "client", false)
                    .await;
                return;
            }
            if let Some(worker) = workers.worker(session_id) {
                worker.cancel_pending_extension_ui_requests();
            }
            if let Err(error) = workers.close_marked(session_id).await {
                eprintln!("senpi rpc close for session {session_id} failed: {error}");
            }
            self.forget_session(session_id);
            return;
        }
        let claim = {
            let mut registry = self.registry.lock().await;
            registry.mark_close(session_id, true)
        };
        let Ok(claim) = claim else {
            return;
        };
        // senpi `releaseOwnedSession`: only a release that leaves the session running is a detach.
        if claim.finalizer != Some(true) {
            self.publish_status(session_id, "detach", "client", false)
                .await;
            return;
        }
        if let Some(binding) = self.bindings.lock().await.remove(session_id) {
            binding.cancel_pending_extension_ui_requests();
            binding.dispose().await;
        }
        let closed = {
            let mut registry = self.registry.lock().await;
            registry
                .close_marked(session_id, self.options.close_grace_ms)
                .await
        };
        if let Err(error) = closed {
            eprintln!("senpi rpc close for session {session_id} failed: {error}");
        }
        self.forget_session(session_id);
    }
    /// Idle eviction (senpi `evictIdleSession`): drains every attachment because eviction ends the
    /// shared session. A RETAINED session with a file is PARKED - `session_parked` names the path it
    /// reopens by - and anything else ends as `session_closed { reason: "idle_evicted" }`.
    async fn evict_idle_session(&self, session_id: &str) {
        if let Some(workers) = &self.workers {
            let record = workers
                .with_entry(session_id, |entry| {
                    self.park_record(
                        entry.close.retain_on_disconnect,
                        entry.session_path.as_deref(),
                        entry.durable_session_id.as_deref(),
                        &entry.profile,
                        entry.context_overlay.as_ref(),
                    )
                })
                .flatten();
            let Some(parked) = workers.force_close(session_id) else {
                return;
            };
            self.write_park_record(record.filter(|_| parked.is_some()))
                .await;
            if let Err(error) = workers.close_marked(session_id).await {
                eprintln!("senpi rpc close for session {session_id} failed: {error}");
            }
            self.publish_eviction(session_id, parked);
            return;
        }
        let (parked, record) = {
            let mut registry = self.registry.lock().await;
            let Some(entry) = registry.get_mut(session_id) else {
                return;
            };
            if entry.close.state != crate::session_registry::RpcSessionState::Open {
                return;
            }
            let parked = if entry.close.retain_on_disconnect {
                entry.session_path.clone()
            } else {
                None
            };
            let record = self.park_record(
                entry.close.retain_on_disconnect,
                entry.session_path.as_deref(),
                entry.durable_session_id.as_deref(),
                &entry.profile,
                entry.context_overlay.as_ref(),
            );
            entry.close.attachments = 0;
            entry.close.state = crate::session_registry::RpcSessionState::Closing;
            (parked, record)
        };
        self.write_park_record(record).await;
        if let Some(binding) = self.bindings.lock().await.remove(session_id) {
            binding.cancel_pending_extension_ui_requests();
            binding.dispose().await;
        }
        let closed = {
            let mut registry = self.registry.lock().await;
            registry
                .close_marked(session_id, self.options.close_grace_ms)
                .await
        };
        if let Err(error) = closed {
            eprintln!("senpi rpc close for session {session_id} failed: {error}");
        }
        self.publish_eviction(session_id, parked);
    }
    /// The eviction's terminal record: a retained file parks, anything else closes as `idle_evicted`.
    fn publish_eviction(&self, session_id: &str, parked: Option<String>) {
        let record = match parked {
            Some(session_path) => {
                serde_json::to_value(crate::rpc_types::RpcSessionLifecycleEvent::SessionParked {
                    session_id: session_id.into(),
                    session_path,
                    reason: Some("idle".into()),
                })
            }
            None => {
                serde_json::to_value(crate::rpc_types::RpcSessionLifecycleEvent::SessionClosed {
                    session_id: session_id.into(),
                    reason: Some("idle_evicted".into()),
                    session_path: None,
                })
            }
        };
        if let Ok(value) = record {
            self.route(session_id, None, value);
        }
        self.forget_session(session_id);
    }
    /// The occupancy sweep (senpi `sweepIdleSessions` on the router's interval): evicts open
    /// sessions idle past the window (halved under memory pressure; busy work restarts the clock),
    /// and answers `true` once the registry stayed empty for the empty-exit window with the exit
    /// permitted. `false` means the host stopped it.
    pub async fn run_idle_policy(
        self: &std::sync::Arc<Self>,
        policy: HostIdlePolicy,
        can_exit: impl Fn() -> bool,
        mut stopped: tokio::sync::watch::Receiver<bool>,
    ) -> bool {
        let mut idle = crate::session_command_router::SessionIdlePolicy::new(
            policy.idle_eviction_ms,
            policy.empty_exit_ms,
        );
        if !policy.idle_eviction_ms.is_finite() && !policy.empty_exit_ms.is_finite() {
            let _ = stopped.wait_for(|stop| *stop).await;
            return false;
        }
        let period = std::time::Duration::from_secs_f64(
            (policy.idle_eviction_ms.min(policy.empty_exit_ms) / 4.).clamp(20., 5000.) / 1000.,
        );
        let mut timer = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
        timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            if *stopped.borrow() {
                return false;
            }
            tokio::select! {
                changed=stopped.changed()=>{if changed.is_err(){return false;}},
                _=timer.tick()=>{
                    idle.memory_pressure=self.memory_pressure.load(std::sync::atomic::Ordering::SeqCst);
                    let now=crate::connection_handler::now_ms();
                    let(mut sessions,size)=if let Some(workers)=&self.workers{
                        let sessions=workers.list_sessions(true).iter().filter_map(|row|{let id=row["sessionId"].as_str()?;workers.with_entry(id,|entry|crate::session_command_router::IdleSession{session_id:id.to_owned(),open:entry.close.state==crate::session_registry::RpcSessionState::Open,busy:entry.worker.busy(),last_command_at:entry.last_command_at})}).collect::<Vec<_>>();
                        (sessions,workers.size())
                    }else{let registry=self.registry.lock().await;
                        let sessions=registry.list_sessions(true).iter().filter_map(|row|{let id=row["sessionId"].as_str()?;let entry=registry.get(id)?;Some(crate::session_command_router::IdleSession{session_id:id.to_owned(),open:entry.close.state==crate::session_registry::RpcSessionState::Open,busy:entry.runtime.as_ref().is_some_and(|runtime|runtime.session().is_session_busy()),last_command_at:entry.last_command_at})}).collect::<Vec<_>>();
                        (sessions,registry.size())};
                    let sweep=idle.sweep(now,&mut sessions,size,can_exit());
                    if let Some(workers)=&self.workers{for session in sessions.iter().filter(|session|session.busy){workers.with_entry(&session.session_id,|entry|entry.last_command_at=session.last_command_at);}}
                    else{let mut registry=self.registry.lock().await;for session in sessions.iter().filter(|session|session.busy){if let Some(entry)=registry.get_mut(&session.session_id){entry.last_command_at=session.last_command_at;}}}
                    for session_id in sweep.evict{let core=std::sync::Arc::clone(self);tokio::spawn(async move{core.evict_idle_session(&session_id).await;});}
                    if sweep.exit{return true;}
                }
            }
        }
    }
    /// Arms the host's self-observation (senpi `startHostObservers`): loop-stall records blame the
    /// attributed session, RSS pressure halves idle parking, and above the refuse watermark the
    /// registry declines NEW worker sessions until a later sample clears it.
    pub fn start_host_observers(
        self: &std::sync::Arc<Self>,
        stopped: tokio::sync::watch::Receiver<bool>,
    ) -> tokio::task::JoinHandle<()> {
        let core = std::sync::Arc::clone(self);
        tokio::spawn(async move {
            let env: HashMap<String, String> = std::env::vars().collect();
            let reader = std::sync::Arc::clone(&core);
            let publisher = std::sync::Arc::clone(&core);
            let memory = move || {
                // Only RSS is consumed, so skip the per-process fd walk of `read_host_process_metrics`.
                let rows =
                    crate::host_process_table::load_process_table_reader(std::env::consts::OS)
                        .and_then(|read| read());
                let rss_mb = rows
                    .map_or_else(
                        crate::host_process_metrics::HostProcessMetrics::default,
                        |rows| {
                            crate::host_process_metrics::metrics_from_rows(
                                &rows,
                                std::process::id(),
                                None,
                            )
                        },
                    )
                    .rss_mb
                    .and_then(|mb| u64::try_from(mb).ok())
                    .unwrap_or(0);
                // A held registry lock means the host is serving sessions right now.
                let sessions = match &reader.workers {
                    Some(workers) => workers.size() as u64,
                    None => reader
                        .registry
                        .try_lock()
                        .map_or(1, |registry| registry.size() as u64),
                };
                (
                    rss_mb * 1_048_576,
                    sessions,
                    crate::connection_handler::now_ms() as u64,
                )
            };
            let publish = move |sample: HostMonitorSample| match sample {
                HostMonitorSample::Lag(sample) => {
                    if let Some(record) = sample.record {
                        publisher.broadcast_record(&record);
                    }
                    if let Some(log) = sample.log {
                        eprint!("{log}");
                    }
                }
                HostMonitorSample::Memory(sample) => {
                    if let Some(pressure) = sample.pressure_change {
                        publisher
                            .memory_pressure
                            .store(pressure, std::sync::atomic::Ordering::SeqCst);
                    }
                    if let Some(rss_mb) = sample.idle_pressure {
                        eprintln!(
                            "senpi rpc host: memory pressure with no sessions: rssMb={rss_mb}"
                        );
                    }
                    if let Some(record) = sample.record {
                        publisher.broadcast_record(&record);
                    }
                    if let Some(log) = sample.log {
                        eprint!("{log}");
                    }
                }
            };
            // senpi `createZeroSessionTrim` + `onSessionCountChange: (size) => trim.observe(size)`.
            let count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(
                core.session_count().await,
            ));
            // The record leaves through a channel: `check` emits under the trimmer lock, which a registry size
            // change takes while it holds its own state, so the broadcast never runs under either.
            let (trimmed, mut trimmed_records) = tokio::sync::mpsc::unbounded_channel();
            let trimmer = std::sync::Arc::new(std::sync::Mutex::new(create_zero_session_trim(
                move |record| {
                    let _ = trimmed.send(record);
                },
                {
                    let count = std::sync::Arc::clone(&count);
                    move || count.load(std::sync::atomic::Ordering::SeqCst)
                },
            )));
            trimmer
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .start();
            let trim_wake = std::sync::Arc::new(tokio::sync::Notify::new());
            let hook: crate::session_registry::SizeChangeHook = {
                let (count, trimmer, wake) = (
                    std::sync::Arc::clone(&count),
                    std::sync::Arc::clone(&trimmer),
                    std::sync::Arc::clone(&trim_wake),
                );
                std::sync::Arc::new(move |size| {
                    count.store(size, std::sync::atomic::Ordering::SeqCst);
                    trimmer
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .observe(size);
                    wake.notify_one();
                })
            };
            match &core.workers {
                Some(workers) => workers.set_on_size_change(hook),
                None => core.registry.lock().await.on_size_change = Some(hook),
            }
            let trim = crate::host_zero_session_trim::run_zero_session_trimmer(
                std::sync::Arc::clone(&trimmer),
                trim_wake,
                stopped.clone(),
            );
            let broadcaster = std::sync::Arc::downgrade(&core);
            let mut broadcast_stopped = stopped.clone();
            let broadcast = async move {
                loop {
                    tokio::select! {
                        record = trimmed_records.recv() => match (record, broadcaster.upgrade()) {
                            (Some(record), Some(core)) => core.broadcast_record(&record),
                            _ => break,
                        },
                        changed = broadcast_stopped.wait_for(|stop| *stop) => {
                            let _ = changed;
                            break;
                        }
                    }
                }
            };
            let monitors = run_host_monitors(
                &env,
                &core.activity,
                &core.blocked,
                memory,
                crate::connection_handler::now_ms,
                publish,
                stopped,
            );
            tokio::join!(monitors, trim, broadcast);
            trimmer
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .stop();
        })
    }
    /// Detaches the closing connection's UI/width state (senpi `releaseOwnerAttachment`); the
    /// ownership count itself was released by the close claim.
    /// Whether the registry holds this handle in the `open` state (senpi `registry.peek(id)?.state === "open"`).
    async fn session_open(&self, session_id: &str) -> bool {
        let open = |state| state == crate::session_registry::RpcSessionState::Open;
        match &self.workers {
            Some(workers) => workers
                .with_entry(session_id, |entry| open(entry.close.state))
                .unwrap_or(false),
            None => self
                .registry
                .lock()
                .await
                .get(session_id)
                .is_some_and(|entry| open(entry.close.state)),
        }
    }
    /// The durable id an open handle runs now and whether it is open with a live binding (senpi
    /// `entry.runtime?.session.sessionId ?? entry.worker?.snapshot?.state.sessionId ?? entry.durableSessionId`).
    async fn live_identity(&self, session_id: &str) -> Option<(bool, Option<String>, i64)> {
        if let Some(workers) = &self.workers {
            return workers.with_entry(session_id, |entry| {
                let durable = entry
                    .worker
                    .snapshot()
                    .and_then(|snapshot| snapshot.state["sessionId"].as_str().map(str::to_owned))
                    .or_else(|| entry.durable_session_id.clone());
                (
                    entry.close.state == crate::session_registry::RpcSessionState::Open,
                    durable,
                    entry.close.attachments,
                )
            });
        }
        let bound = self.bindings.lock().await.contains_key(session_id);
        let registry = self.registry.lock().await;
        registry.get(session_id).map(|entry| {
            let durable = entry
                .runtime
                .as_ref()
                .map(|runtime| runtime.session().session_id())
                .or_else(|| entry.durable_session_id.clone());
            (
                bound && entry.close.state == crate::session_registry::RpcSessionState::Open,
                durable,
                entry.close.attachments,
            )
        })
    }
    /// senpi `attachLiveSession` + `recordAttachment`: a synchronous refcount change that never opens a path and
    /// never applies a launch profile; ending a retained session's parked window tells it `session_resumed`.
    async fn attach_live(&self, owner: &str, session_id: &str) -> Result<(), &'static str> {
        let now = crate::connection_handler::now_ms();
        if let Some(workers) = &self.workers {
            workers
                .attach_session(session_id, now)
                .map_err(|_| crate::rpc_types::RPC_ERROR_UNKNOWN_SESSION)?;
        } else {
            let resumed = self
                .registry
                .lock()
                .await
                .attach_session(session_id, now)
                .map_err(|_| crate::rpc_types::RPC_ERROR_UNKNOWN_SESSION)?;
            if let Some(runtime) = resumed {
                runtime
                    .emit_attachment_event(
                        maho_core::agent_session_runtime::AttachmentEvent::SessionResumed,
                    )
                    .await;
            }
        }
        if self.workers.is_some() {
            let mut fanout = self
                .fanout
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let mut declared = None;
            if fanout.targets.capabilities(owner).is_none() {
                let capabilities = self
                    .pending_capabilities
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .get(owner)
                    .cloned()
                    .unwrap_or_else(|| self.options.capabilities.clone());
                if let Err(error) = fanout.set_capabilities(owner, &capabilities) {
                    eprintln!("senpi rpc connection {owner} failed: {error}");
                }
                declared = Some(capabilities);
            }
            if let Err(error) = fanout.attach(owner, session_id, now) {
                eprintln!("senpi rpc connection {owner} failed: {error}");
            }
            drop(fanout);
            // senpi `setConnectionCapabilities` always withdraws a non-presenting connection's grants.
            if let Some(capabilities) = declared {
                self.presenters.capabilities_changed(owner, &capabilities);
            }
            self.attachments
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .attach(owner, session_id);
            self.presenters.attach(owner, session_id);
        } else {
            self.attach_connection(Some(owner), session_id).await;
        }
        Ok(())
    }
    /// senpi `attachSession`.
    async fn attach_session(
        &self,
        connection: Option<&str>,
        session_id: &str,
        durable_session_id: &serde_json::Value,
        expected_host_instance: &serde_json::Value,
    ) -> Result<(), (&'static str, serde_json::Value)> {
        if self.is_draining() {
            return Err((
                crate::rpc_types::RPC_ERROR_SESSION_BUSY_LIFECYCLE,
                crate::session_attachment_commands::draining_refusal_detail(),
            ));
        }
        let unknown = || {
            (
                crate::rpc_types::RPC_ERROR_UNKNOWN_SESSION,
                serde_json::json!({}),
            )
        };
        let Some((true, durable, _)) = self.live_identity(session_id).await else {
            return Err(unknown());
        };
        if !crate::session_attachment_commands::identity_matches(
            durable.as_deref(),
            durable_session_id,
            expected_host_instance,
            crate::protocol_identity::host_instance_id(),
        ) {
            return Err((
                crate::rpc_types::RPC_ERROR_SESSION_IDENTITY_MISMATCH,
                serde_json::json!({}),
            ));
        }
        let Some(owner) = connection else {
            return Err((
                crate::rpc_types::RPC_ERROR_SESSION_NOT_ATTACHED,
                serde_json::json!({"reason":"no_connection"}),
            ));
        };
        self.attach_live(owner, session_id)
            .await
            .map_err(|code| (code, serde_json::json!({})))?;
        // senpi `attach`: every live attachment is published.
        self.publish_status(session_id, "attach", "client", false)
            .await;
        Ok(())
    }
    /// senpi `releaseAttachment`: gives up one of the caller's attachments; the session ends only when that was the
    /// last one, and a turn in flight runs to settlement first (`deferred: true`). A drain owns every outstanding
    /// physical hold until its park, so it is never preempted.
    async fn release_attachment(
        self: &std::sync::Arc<Self>,
        connection: Option<&str>,
        id: Option<String>,
        session_id: String,
    ) {
        let owned = connection.is_some_and(|owner| {
            self.attachments
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .owned_by(owner)
                .iter()
                .any(|(owned, _)| *owned == session_id)
        });
        let (Some(owner), true) = (connection, owned) else {
            self.refuse(
                connection,
                id,
                "release_attachment",
                crate::rpc_types::RPC_ERROR_SESSION_NOT_ATTACHED,
                serde_json::json!({"reason":"not_attached"}),
                Some(session_id),
            );
            return;
        };
        let last = self
            .attachments
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .owned_by(owner)
            .iter()
            .any(|(owned, count)| *owned == session_id && *count == 1);
        self.attachments
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .release(owner, &session_id);
        if last {
            self.release_owner_attachment(owner, &session_id).await;
        }
        // The drain owns every outstanding physical hold until its park; never preempt it.
        let deferred = !self.is_draining() && self.release_disconnect_attachment(&session_id).await;
        self.success(
            connection,
            id,
            "release_attachment",
            Some(crate::session_attachment_commands::release_reply(deferred)),
            Some(session_id),
        );
    }
    /// senpi `releaseDisconnectAttachment`: only the release that would end the session can abort its turn, so that
    /// one is deferred - never skipped - until the turn settles, from a task no connection owns. `true` means the
    /// settlement owns the final release; otherwise the release has completed before this returns.
    async fn release_disconnect_attachment(self: &std::sync::Arc<Self>, session_id: &str) -> bool {
        let ends_session = |retained: bool, attachments: i64| !retained && attachments <= 1;
        if let Some(workers) = self.workers.clone() {
            let busy = workers
                .with_entry(session_id, |entry| {
                    entry.close.state == crate::session_registry::RpcSessionState::Open
                        && ends_session(entry.close.retain_on_disconnect, entry.close.attachments)
                })
                .unwrap_or(false)
                .then(|| workers.worker(session_id))
                .flatten()
                .filter(|worker| worker.busy());
            if let Some(worker) = busy {
                self.release_worker_after_settle(worker, session_id.to_owned());
                return true;
            }
            self.release_owned_session(session_id).await;
            return false;
        }
        let streaming = {
            let registry = self.registry.lock().await;
            registry
                .get(session_id)
                .filter(|entry| {
                    entry.close.state == crate::session_registry::RpcSessionState::Open
                        && ends_session(entry.close.retain_on_disconnect, entry.close.attachments)
                })
                .and_then(|entry| entry.runtime.as_ref())
                .map(|runtime| runtime.session().clone())
                .filter(maho_core::agent_session::AgentSession::is_streaming)
        };
        if let Some(session) = streaming {
            self.release_after_settle(session, session_id.to_owned());
            return true;
        }
        self.release_owned_session(session_id).await;
        false
    }
    /// senpi `reclaimSession`: ends the live session holding `sessionPath` for a caller that names its durable id
    /// and this host, once no other connection is attached. Admission, the visible attach and the claim of ALL the
    /// caller's holds happen without an await between them that another command could interleave with.
    async fn reclaim_session(
        &self,
        connection: Option<&str>,
        session_path: &serde_json::Value,
        durable_session_id: &serde_json::Value,
        expected_host_instance: &serde_json::Value,
    ) -> Result<String, (&'static str, serde_json::Value)> {
        if self.is_draining() {
            return Err((
                crate::rpc_types::RPC_ERROR_SESSION_BUSY_LIFECYCLE,
                crate::session_attachment_commands::draining_refusal_detail(),
            ));
        }
        let unknown = || {
            (
                crate::rpc_types::RPC_ERROR_UNKNOWN_SESSION,
                serde_json::json!({}),
            )
        };
        let Some(path) = session_path.as_str().filter(|path| !path.is_empty()) else {
            return Err(unknown());
        };
        let canonical = |path: &str| {
            crate::session_registry::canonical_path(std::path::Path::new(path))
                .unwrap_or_else(|_| std::path::PathBuf::from(path))
        };
        let key = canonical(path);
        let session_id = self
            .list_rows(true)
            .await
            .into_iter()
            .find(|row| {
                row["sessionPath"]
                    .as_str()
                    .is_some_and(|path| canonical(path) == key)
            })
            .and_then(|row| row["sessionId"].as_str().map(str::to_owned))
            .ok_or_else(unknown)?;
        let Some((open, durable, attachments)) = self.live_identity(&session_id).await else {
            return Err(unknown());
        };
        if !open {
            return Err((
                crate::rpc_types::RPC_ERROR_SESSION_BUSY_LIFECYCLE,
                serde_json::json!({}),
            ));
        }
        if !expected_host_instance.is_string()
            || !crate::session_attachment_commands::identity_matches(
                durable.as_deref(),
                durable_session_id,
                expected_host_instance,
                crate::protocol_identity::host_instance_id(),
            )
        {
            return Err((
                crate::rpc_types::RPC_ERROR_SESSION_IDENTITY_MISMATCH,
                serde_json::json!({}),
            ));
        }
        let Some(owner) = connection else {
            return Err((
                crate::rpc_types::RPC_ERROR_SESSION_NOT_ATTACHED,
                serde_json::json!({"reason":"no_connection"}),
            ));
        };
        let own = self
            .attachments
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .owned_by(owner)
            .into_iter()
            .find_map(|(owned, count)| (owned == session_id).then_some(count))
            .unwrap_or(0);
        let elsewhere = crate::session_attachment_commands::attached_elsewhere(attachments, own);
        if elsewhere > 0 {
            return Err((
                crate::rpc_types::RPC_ERROR_ATTACHED_ELSEWHERE,
                serde_json::json!({"attachments":elsewhere}),
            ));
        }
        self.attach_live(owner, &session_id)
            .await
            .map_err(|code| (code, serde_json::json!({})))?;
        self.publish_status(&session_id, "attach", "client", false)
            .await;
        // claimClose { drainAttachments }: every outstanding hold of the caller goes with this one claim.
        let finalizer = loop {
            let claim = match &self.workers {
                Some(workers) => workers.mark_close(&session_id, false),
                None => self.registry.lock().await.mark_close(&session_id, false),
            };
            match claim {
                Ok(claim) => {
                    if let Some(finalizer) = claim.finalizer {
                        break finalizer;
                    }
                }
                Err(_) => break false,
            }
        };
        {
            let mut attachments = self
                .attachments
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            while attachments.owns(Some(owner), &session_id) {
                attachments.release(owner, &session_id);
            }
        }
        // The caller stays in the fanout until the seal: the terminal record is delivered to attached connections.
        self.connection_left(&session_id, owner).await;
        self.widths
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear_width(&session_id, Some(owner));
        if let Some(dir) = self.handover_parked_dir.get() {
            crate::host_handover_parked::clear_handover_parked_session(dir, path).await;
        }
        if finalizer {
            if let Some(workers) = &self.workers {
                if let Some(worker) = workers.worker(&session_id) {
                    worker.cancel_pending_extension_ui_requests();
                    worker.dispose();
                }
                if let Err(error) = workers.close_marked(&session_id).await {
                    eprintln!("senpi rpc close for session {session_id} failed: {error}");
                }
            } else {
                if let Some(binding) = self.bindings.lock().await.remove(&session_id) {
                    binding.cancel_pending_extension_ui_requests();
                    binding.dispose().await;
                }
                let closed = self
                    .registry
                    .lock()
                    .await
                    .close_marked(&session_id, self.options.close_grace_ms)
                    .await;
                if let Err(error) = closed {
                    eprintln!("senpi rpc close for session {session_id} failed: {error}");
                }
            }
            if let Ok(value) =
                serde_json::to_value(crate::rpc_types::RpcSessionLifecycleEvent::SessionClosed {
                    session_id: session_id.clone(),
                    reason: Some("client_close".into()),
                    session_path: None,
                })
            {
                // senpi `sealLifecycle(..., attachedOnly = true)`: never broadcast to unattached connections.
                let stdio = self
                    .fanout
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .deliver_to_attached(&session_id, value);
                match stdio {
                    Ok(Some(record)) => {
                        let _ = self.default_writer.enqueue(&session_id, record);
                    }
                    Ok(None) => {}
                    Err(error) => {
                        eprintln!("senpi rpc session {session_id} output failed: {error}")
                    }
                }
            }
        }
        self.fanout
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .detach(owner, &session_id);
        self.forget_session(&session_id);
        Ok(session_id)
    }
    /// senpi `connectionLeft`: whenever a connection stops attaching (owner release, socket drop, reclaim), a
    /// relocation request asked of that connection ends with it.
    async fn connection_left(&self, session_id: &str, connection: &str) {
        let binding = self.bindings.lock().await.get(session_id).cloned();
        if let Some(binding) = binding {
            binding.connection_detached(connection);
        }
    }
    async fn release_owner_attachment(&self, owner: &str, session_id: &str) {
        self.presenters.detach(owner, session_id);
        self.connection_left(session_id, owner).await;
        self.widths
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear_width(session_id, Some(owner));
        self.fanout
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .detach(owner, session_id);
    }
}
type OpensByConnection =
    std::collections::BTreeMap<String, Vec<tokio::sync::watch::Receiver<bool>>>;
/// One open's barrier (senpi `openWithBarrier`'s `finally`): dropping it settles the open and forgets it.
struct OpenBarrier<'a> {
    host: &'a HostCore,
    owner: String,
    settle: tokio::sync::watch::Sender<bool>,
}
impl Drop for OpenBarrier<'_> {
    fn drop(&mut self) {
        let _ = self.settle.send(true);
        let mut opens = self
            .host
            .opens_by_connection
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let own = self.settle.subscribe();
        if let Some(pending) = opens.get_mut(&self.owner) {
            pending.retain(|settled| !settled.same_channel(&own));
            if pending.is_empty() {
                opens.remove(&self.owner);
            }
        }
    }
}
/// The writer's side of the worker grant bridge (senpi `SessionEventWriter`'s `settleGrantDialog` /
/// `acceptWorkerSecuritySnapshot` over `SessionEventFanout`): a worker's grant dialog is routed by the connections
/// this host has attached to the session, never by the worker.
struct HostSecurityWriter {
    fanout: std::sync::Arc<std::sync::Mutex<crate::session_event_fanout::SessionEventFanout>>,
}

impl crate::session_worker_client::WorkerSecurityWriter for HostSecurityWriter {
    fn attached(&self, session_id: &str) -> Vec<String> {
        self.fanout
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .targets
            .attached_connections(session_id)
    }
    fn has_capability(&self, connection: &str, capability: &str) -> bool {
        self.fanout
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .targets
            .connection_has(connection, capability)
    }
    fn is_empty(&self) -> bool {
        self.fanout
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .targets
            .is_empty()
    }
    fn settle_grant_dialog(&self, _session_id: &str, _id: &str) {
        // The dialog this settles lives in the security presentations (senpi `security-presentations.ts`), which
        // this host does not hold yet; the worker's own settle is what ends it until that port lands.
    }
    fn accept_worker_security_snapshot(&self, _session_id: &str, _snapshot: &serde_json::Value) {
        // Same seam: the snapshot becomes presentation state (and a replayed `notify` per notice) once the security
        // presentations are ported; validating it here is what keeps an unreadable one out of this host.
    }
}

/// The router's side of `release_session` (senpi `releaseSession(port, command)` wiring in `handle`).
struct HostReleasePort<'a> {
    core: &'a HostCore,
    /// The connection that asked, so its own attachment never counts as another client.
    connection: Option<&'a str>,
}
#[async_trait::async_trait]
impl crate::session_release_interrupt::ReleaseSettlePort for HostReleasePort<'_> {
    async fn other_requests_settled(&self, session_id: &str) {
        loop {
            let notified = self.core.request_settled.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.core.active_request_count(session_id) <= 1 {
                return;
            }
            notified.await;
        }
    }
    fn pending_prompts(
        &self,
        _session_id: &str,
    ) -> Vec<crate::session_release_interrupt::BoxFuture<'static, ()>> {
        // The binding does not track `prompt` calls apart from the session's own in-flight count, which
        // `activity_snapshot` and `wait_for_idle` already cover.
        Vec::new()
    }
}
#[async_trait::async_trait]
impl crate::session_release::SessionReleasePort for HostReleasePort<'_> {
    fn draining(&self) -> bool {
        self.core.is_draining()
    }
    fn host_instance(&self) -> Option<String> {
        Some(crate::protocol_identity::host_instance_id().to_owned())
    }
    async fn lookup(
        &self,
        session_id: &str,
    ) -> Result<crate::session_release::ReleaseEntry, crate::session_release::LookupFailure> {
        let now = crate::connection_handler::now_ms();
        let lookup_failure = |error: crate::session_registry::RpcSessionRegistryError| {
            crate::session_release::LookupFailure(error.code)
        };
        let attachments = |count: i64| u64::try_from(count.max(0)).unwrap_or(0);
        if let Some(workers) = &self.core.workers {
            workers
                .get_for_command(session_id, "release_session", now)
                .map_err(lookup_failure)?;
            return workers
                .with_entry(session_id, |entry| crate::session_release::ReleaseEntry {
                    open: entry.close.state == crate::session_registry::RpcSessionState::Open,
                    session: None,
                    session_path: entry.session_path.clone(),
                    attachments: attachments(entry.close.attachments),
                })
                .ok_or_else(|| {
                    crate::session_release::LookupFailure(
                        crate::rpc_types::RPC_ERROR_UNKNOWN_SESSION.to_owned(),
                    )
                });
        }
        let mut registry = self.core.registry.lock().await;
        let entry = registry
            .get_for_command(session_id, "release_session", now)
            .map_err(lookup_failure)?;
        let session = entry.runtime.as_ref().map(|runtime| {
            std::sync::Arc::new(crate::session_release_adapter::AgentSessionRelease::new(
                runtime.session().clone(),
            ))
                as std::sync::Arc<dyn crate::session_release_interrupt::ReleasableSession>
        });
        let session_path = entry.session_path.clone().or_else(|| {
            entry
                .runtime
                .as_ref()
                .and_then(|runtime| runtime.session().session_file())
        });
        Ok(crate::session_release::ReleaseEntry {
            open: entry.close.state == crate::session_registry::RpcSessionState::Open,
            session,
            session_path,
            attachments: attachments(entry.close.attachments),
        })
    }
    fn other_requests(&self, session_id: &str) -> usize {
        // The release is itself one of the session's in-flight requests.
        self.core.active_request_count(session_id).saturating_sub(1)
    }
    fn other_attachments(&self, session_id: &str) -> u64 {
        // The requesting connection's own attachment is not another client: it is drained with the rest.
        let fanout = self
            .core
            .fanout
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        u64::try_from(
            fanout
                .targets
                .attached_connections(session_id)
                .iter()
                .filter(|id| Some(id.as_str()) != self.connection)
                .count(),
        )
        .unwrap_or(u64::MAX)
    }
    async fn prepare_outgoing_state(
        &self,
        session_path: &str,
        move_id: &str,
    ) -> Result<
        crate::session_release::StagedState,
        crate::session_release_interrupt::ReleaseWriteError,
    > {
        // senpi `await prepareOutgoingState`: the staging walk runs off the async workers.
        let (session_path, move_id) = (session_path.to_owned(), move_id.to_owned());
        tokio::task::spawn_blocking(move || {
            crate::session_release::prepare_outgoing_state(&session_path, &move_id)
        })
        .await
        .map_err(|error| error.to_string())
        .and_then(|result| result)
        .map_err(crate::session_release_interrupt::ReleaseWriteError::Other)
    }
    fn discard_outgoing_state(&self, dir: &str) {
        // senpi `await rm(dir, { recursive, force })`. SAFETY-DIVERGENCE(senpi releaseSession): the discard runs from a
        // drop guard that cannot await, so the removal is detached; a retry stages a fresh attempt folder and
        // `prepare_outgoing_state` removes any dead attempt left behind, treating one this discard already removed
        // as gone (senpi `force: true`), so the reply never depends on it.
        let dir = dir.to_owned();
        tokio::task::spawn_blocking(move || crate::session_release::discard_outgoing_state(&dir));
    }
    async fn tear_down(&self, session_id: &str, session_path: &str) -> bool {
        self.core.tear_down_released(session_id, session_path).await
    }
}
/// The router's side of `unload_session` (senpi `unloadSession(port, command)` wiring in `handle`): the release port's
/// request and attachment counts, plus the unload's own lookup, child count and park teardown.
struct HostUnloadPort<'a> {
    release: HostReleasePort<'a>,
}
#[async_trait::async_trait]
impl crate::session_release_interrupt::ReleaseSettlePort for HostUnloadPort<'_> {
    async fn other_requests_settled(&self, session_id: &str) {
        crate::session_release_interrupt::ReleaseSettlePort::other_requests_settled(
            &self.release,
            session_id,
        )
        .await;
    }
    fn pending_prompts(
        &self,
        session_id: &str,
    ) -> Vec<crate::session_release_interrupt::BoxFuture<'static, ()>> {
        crate::session_release_interrupt::ReleaseSettlePort::pending_prompts(
            &self.release,
            session_id,
        )
    }
}
#[async_trait::async_trait]
impl crate::session_unload::SessionUnloadPort for HostUnloadPort<'_> {
    fn draining(&self) -> bool {
        self.release.core.is_draining()
    }
    fn by(&self) -> crate::effect_record::EffectActor {
        crate::effect_record::EffectActor {
            connection_id: self.release.connection.unwrap_or("host").to_owned(),
        }
    }
    async fn lookup(&self, session_id: &str) -> Result<crate::session_unload::UnloadEntry, String> {
        let now = crate::connection_handler::now_ms();
        if let Some(workers) = &self.release.core.workers {
            workers
                .get_for_command(session_id, "unload_session", now)
                .map_err(|error| error.code)?;
            return workers
                .with_entry(session_id, |entry| crate::session_unload::UnloadEntry {
                    open: entry.close.state == crate::session_registry::RpcSessionState::Open,
                    session: None,
                    session_path: entry.session_path.clone(),
                })
                .ok_or_else(|| crate::rpc_types::RPC_ERROR_UNKNOWN_SESSION.to_owned());
        }
        let mut registry = self.release.core.registry.lock().await;
        let entry = registry
            .get_for_command(session_id, "unload_session", now)
            .map_err(|error| error.code)?;
        let session = entry.runtime.as_ref().map(|runtime| {
            std::sync::Arc::new(crate::session_release_adapter::AgentSessionRelease::new(
                runtime.session().clone(),
            )) as std::sync::Arc<dyn crate::session_unload::UnloadableSession>
        });
        Ok(crate::session_unload::UnloadEntry {
            open: entry.close.state == crate::session_registry::RpcSessionState::Open,
            session,
            session_path: entry.session_path.clone(),
        })
    }
    fn other_requests(&self, session_id: &str) -> usize {
        crate::session_release::SessionReleasePort::other_requests(&self.release, session_id)
    }
    fn other_attachments(&self, session_id: &str) -> u64 {
        crate::session_release::SessionReleasePort::other_attachments(&self.release, session_id)
    }
    async fn open_child_sessions(&self, durable_session_id: &str) -> usize {
        let rows = match &self.release.core.workers {
            Some(workers) => workers.list_sessions(true),
            None => self.release.core.registry.lock().await.list_sessions(true),
        };
        open_child_session_count(&rows, durable_session_id)
    }
    async fn tear_down(&self, session_id: &str, session_path: &str) -> bool {
        self.release
            .core
            .tear_down_unloaded(session_id, session_path)
            .await
    }
}
/// The session commands that replace the live session in place (senpi `SessionRuntime` new/switch/fork/import).
fn replaces_session(command: &serde_json::Value) -> bool {
    matches!(
        command["type"].as_str(),
        Some("new_session" | "switch_session" | "fork" | "clone" | "import_jsonl")
    )
}
/// Listed sessions other than `durable_session_id` itself whose context names it as parent or root.
fn open_child_session_count(rows: &[serde_json::Value], durable_session_id: &str) -> usize {
    rows.iter()
        .filter(|row| {
            row["durableSessionId"] != durable_session_id
                && (row["context"]["parent_session_id"] == durable_session_id
                    || row["context"]["root_session_id"] == durable_session_id)
        })
        .count()
}
tokio::task_local! {pub(crate) static CURRENT_CONNECTION:Option<String>;}
/// The presenter control endpoint's view of this host's writer (senpi `SessionEventWriter` presenter getters).
struct PresenterWriter {
    presenters: std::sync::Arc<crate::session_event_writer::SessionMcpPresenters>,
    fanout: std::sync::Arc<std::sync::Mutex<crate::session_event_fanout::SessionEventFanout>>,
}
impl crate::mcp_presenter_control::McpPresenterWriter for PresenterWriter {
    fn mcp_form_binding(
        &self,
        session_id: &str,
    ) -> Option<std::sync::Arc<crate::mcp_form_types::McpFormBinding>> {
        self.presenters.binding(session_id)
    }
    fn find_mcp_presenter_connection(
        &self,
        nonce: &str,
    ) -> Option<crate::mcp_presenter_authority::McpPresenterConnection> {
        self.presenters.find_connection(nonce)
    }
    fn connection_capabilities(&self, connection_id: &str) -> Option<Vec<String>> {
        self.fanout
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .targets
            .capabilities(connection_id)
            .map(<[String]>::to_vec)
    }
}
#[cfg(unix)]
mod tempfile_dir {
    /// senpi `mkdtemp(join(tmpdir(), "senpi-presenter-"))`, removed with the host.
    pub struct PresenterDir {
        pub path: std::path::PathBuf,
    }
    impl PresenterDir {
        pub fn create() -> Option<Self> {
            let path =
                std::env::temp_dir().join(format!("senpi-presenter-{}", uuid::Uuid::new_v4()));
            crate::host_daemon_paths::create_private_directory(&path).ok()?;
            Some(Self { path })
        }
    }
    impl Drop for PresenterDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}
/// Writes one `mcp_form_reply` raw to the sender's control lane; a failed write detaches its presenter (senpi
/// `handleMcpForm`). A stdio host (no connection) answers on its default writer.
fn write_presenter_reply(
    fanout: &std::sync::Weak<std::sync::Mutex<crate::session_event_fanout::SessionEventFanout>>,
    writer: &crate::session_event_writer::SessionWriterActor,
    presenters: &std::sync::Weak<crate::session_event_writer::SessionMcpPresenters>,
    connection: Option<&str>,
    reply: &serde_json::Value,
) {
    let Some(connection) = connection else {
        let _ = writer.enqueue("", reply.clone());
        return;
    };
    let written = fanout.upgrade().is_some_and(|fanout| {
        fanout
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .enqueue_raw(connection, reply)
    });
    if !written && let Some(presenters) = presenters.upgrade() {
        presenters.reply_failed(connection);
    }
}
/// Routes one session record (senpi `SessionEventWriter.enqueue`): responses, bash updates and
/// blocking UI requests go to their requester only; content goes to the connections attached to
/// the session and is remembered for late joiners; lifecycle records follow their visibility
/// rule. With no registered connection the record takes the shared stdio lane.
fn route_session_record(
    fanout: &std::sync::Mutex<crate::session_event_fanout::SessionEventFanout>,
    writer: &crate::session_event_writer::SessionWriterActor,
    session_id: &str,
    owner: Option<&str>,
    mut record: serde_json::Value,
) {
    // Every copy, socket or stdio, is stamped where the host enqueues it (senpi `observeLifecycleDelivery`).
    crate::lifecycle_delivery::observe_lifecycle_delivery(&mut record);
    // senpi `SessionEventWriter.enqueue`: a `snapshot_marker` goes only to the requester's own mirror subscription,
    // never to attachments, the attach replay or stdio. This host has no mirror subscriptions yet (session status
    // stream open), so no target asked for it.
    if record["type"] == crate::get_snapshot::SNAPSHOT_MARKER {
        return;
    }
    let targeted = matches!(
        record["type"].as_str(),
        Some("response" | "bash_execution_update")
    ) || (record["type"] == "extension_ui_request"
        && matches!(
            record["method"].as_str(),
            Some("select" | "confirm" | "input" | "editor")
        ));
    let delivered = fanout
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .deliver_owned(session_id, owner, targeted, record);
    match delivered {
        Ok(stdio) => {
            for record in stdio {
                if let Err(error) = writer.enqueue(session_id, record) {
                    eprintln!("senpi rpc session {session_id} output failed: {error}");
                }
            }
        }
        Err(error) => eprintln!("senpi rpc session {session_id} output failed: {error}"),
    }
}
#[cfg(test)]
mod handoff_tests {
    use super::*;
    #[test]
    fn presenter_pipe_paths_are_private_random_pipe_names() {
        let first = presenter_pipe_path();
        assert!(first.starts_with(r"\\.\pipe\senpi-presenter-"));
        assert_eq!(first.len(), r"\\.\pipe\senpi-presenter-".len() + 36);
        assert_ne!(first, presenter_pipe_path());
        assert!(!first.contains(crate::protocol_identity::host_instance_id()));
    }
    fn idle_core() -> std::sync::Arc<HostCore> {
        let create_runtime: HostRuntimeFactory = std::sync::Arc::new(
            |_profile: crate::session_registry::RpcSessionLaunchProfile| -> std::pin::Pin<
                Box<
                    dyn std::future::Future<
                            Output = Result<
                                maho_core::agent_session_runtime::AgentSessionRuntime,
                                String,
                            >,
                        > + Send,
                >,
            > {
                Box::pin(async { Err("this host opens no session in this test".to_owned()) })
            },
        );
        let writer = std::sync::Arc::new(crate::session_event_writer::SessionWriterActor::new(
            tokio::io::sink(),
        ));
        std::sync::Arc::new(HostCore::new(
            HostCoreOptions {
                agent_dir: std::path::PathBuf::from("/tmp"),
                cwd: "/tmp".to_owned(),
                create_runtime,
                capabilities: vec![
                    crate::custom_capability::EXTENSION_EVENTS_CAPABILITY.to_owned(),
                ],
                close_grace_ms: 10_000,
            },
            writer,
        ))
    }
    #[tokio::test]
    async fn handoff_is_advertised_only_after_its_handler_is_installed() {
        let core = idle_core();
        assert!(!core.advertised_capabilities().iter().any(|capability|capability==crate::host_decision::GENERATION_HANDOFF_CAPABILITY));
        core.enable_handoff();
        assert!(core.advertised_capabilities().iter().any(|capability|capability==crate::host_decision::GENERATION_HANDOFF_CAPABILITY));
    }
    /// senpi `HostCoreGate.answered`: shutdown waits for a `begin_handover` still being answered, and settles at once
    /// when none is.
    #[tokio::test]
    async fn handover_answered_waits_for_an_answer_in_flight() {
        let core = idle_core();
        core.handover_answered().await;
        let answering = HandoverAnswering::new(&core);
        let mut waiting = Box::pin(core.handover_answered());
        assert!(futures::poll!(waiting.as_mut()).is_pending());
        drop(answering);
        waiting.await;
    }
    #[tokio::test]
    async fn begin_handover_reaches_the_gate_and_a_foreign_agent_dir_is_refused() {
        use tokio::io::AsyncBufReadExt;
        let core = idle_core();
        let (write, read) = tokio::io::duplex(1 << 16);
        core.register_connection(
            "c",
            crate::socket_event_fanout::SocketEventSinkActor::new(
                write,
                crate::socket_event_fanout::DEFAULT_QUEUE_BYTES,
                crate::socket_event_fanout::DEFAULT_STALL_MS,
                std::sync::Arc::clone(&core.blocked),
                |_| {},
            ),
        );
        let mut lines = tokio::io::BufReader::new(read).lines();
        let request = |agent_dir: &str| {
            serde_json::json!({"type":"begin_handover","id":"h","operation_id":"op","if_instance_id":"i","if_generation":0,"target_runtime_build_id":"sha256:t","launch":{"command":"x","args":[]},"host_args":[],"env":{},"agent_dir":agent_dir}).to_string()
        };
        // Without the gate the request is refused as unsupported.
        core.handle(Some("c"), &request("/tmp")).await.unwrap();
        let answer: serde_json::Value =
            serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        assert_eq!(answer["data"]["refused"], "handover_unsupported");
        assert!(
            !core
                .advertised_capabilities()
                .iter()
                .any(|capability| capability
                    == crate::host_handover_request::RUNTIME_IDENTITY_HANDOVER_CAPABILITY)
        );
        let temp = tempfile::tempdir().unwrap();
        let ours = temp.path().join("ours");
        let theirs = temp.path().join("theirs");
        std::fs::create_dir_all(&ours).unwrap();
        std::fs::create_dir_all(&theirs).unwrap();
        core.enable_idle_handover(
            crate::host_handover_wire::HostEndpoint {
                agent_dir: ours,
                socket: temp.path().join("s.sock").to_string_lossy().into_owned(),
            },
            "sha256:loaded".into(),
        );
        assert!(
            core.advertised_capabilities()
                .iter()
                .any(|capability| capability
                    == crate::host_handover_request::RUNTIME_IDENTITY_HANDOVER_CAPABILITY)
        );
        core.handle(Some("c"), &request(&theirs.to_string_lossy()))
            .await
            .unwrap();
        let answer: serde_json::Value =
            serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        assert_eq!(answer["id"], "h");
        assert_eq!(answer["success"], true);
        assert_eq!(answer["data"]["refused"], "foreign_agent_dir");
        assert!(core.handover.get().unwrap().handover.view().is_none());
        // An idle host with no sessions and nothing in flight is at its safe idle point.
        assert!(core.is_host_idle());
    }
    /// senpi `withHostIdentity(response, runtimeBuildId, ...)`: the loaded runtime is reported with no idle handover
    /// installed (a Windows host has none).
    #[tokio::test]
    async fn protocol_info_reports_the_runtime_build_id_without_a_handover_gate() {
        use tokio::io::AsyncBufReadExt;
        let core = idle_core();
        let (write, read) = tokio::io::duplex(1 << 16);
        core.register_connection(
            "c",
            crate::socket_event_fanout::SocketEventSinkActor::new(
                write,
                crate::socket_event_fanout::DEFAULT_QUEUE_BYTES,
                crate::socket_event_fanout::DEFAULT_STALL_MS,
                std::sync::Arc::clone(&core.blocked),
                |_| {},
            ),
        );
        let mut lines = tokio::io::BufReader::new(read).lines();
        core.set_runtime_build_id("sha256:loaded".into());
        core.handle(Some("c"), r#"{"id":"p","type":"get_protocol_info"}"#)
            .await
            .unwrap();
        let answer: serde_json::Value =
            serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        assert_eq!(answer["command"], "get_protocol_info");
        assert_eq!(answer["data"]["runtimeBuildId"], "sha256:loaded");
        assert!(answer["data"].get("handover").is_none());
    }
    #[tokio::test]
    async fn protocol_info_advertises_host_capabilities_the_decision_requires() {
        let capabilities = idle_core().advertised_capabilities();
        for required in crate::host_decision::REQUIRED_HOST_CAPABILITIES {
            assert!(
                capabilities.iter().any(|capability| capability == required),
                "{required}"
            );
        }
        assert_eq!(
            capabilities
                .iter()
                .filter(|capability| *capability == "multi_session")
                .count(),
            1
        );
    }
    #[tokio::test]
    async fn presenter_capability_is_advertised_only_under_enforcement_and_private_records_never_echo()
     {
        let core = idle_core();
        assert!(!core.advertised_capabilities().iter().any(
            |capability| capability == crate::host_capabilities::MCP_FORM_PRESENTER_CAPABILITY
        ));
        let Some(core) = std::sync::Arc::try_unwrap(core).ok() else {
            panic!("the idle core is uniquely owned");
        };
        let core = core.with_mcp_presenter_enforcement(true);
        let capabilities = core.advertised_capabilities();
        assert_eq!(
            capabilities
                .iter()
                .filter(|capability| *capability
                    == crate::host_capabilities::MCP_FORM_PRESENTER_CAPABILITY)
                .count(),
            1
        );
        // With presenters on, a private record is answered by the writer and a parse error echoes nothing.
        core.handle(
            None,
            r#"{"type":"mcp_form_sync","id":"p","sessionId":"none"}"#,
        )
        .await
        .unwrap();
        core.handle(None, "{not json").await.unwrap();
    }
    /// senpi presenter-rpc "private resolution and the next form survive an outstanding socket drain": private
    /// frames ride the raw lane, so a peer shed into recovery by an overflowing backlog still receives the request,
    /// its resolution and the next request, and no overflow error ever previews them.
    #[tokio::test]
    async fn private_frames_survive_an_outstanding_socket_drain_recovery_and_overflow() {
        use tokio::io::AsyncBufReadExt;
        let Some(core) = std::sync::Arc::try_unwrap(idle_core()).ok() else {
            panic!("the idle core is uniquely owned");
        };
        let core = core.with_mcp_presenter_enforcement(true);
        let failures = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let failed = std::sync::Arc::clone(&failures);
        // A 64-byte pipe nobody reads until the backlog is queued: the drain stays outstanding.
        let (write, read) = tokio::io::duplex(64);
        core.register_connection(
            "human",
            crate::socket_event_fanout::SocketEventSinkActor::new(
                write,
                256,
                30_000,
                std::sync::Arc::clone(&core.blocked),
                move |error| failed.lock().unwrap().push(error),
            ),
        );
        core.fanout
            .lock()
            .unwrap()
            .set_capabilities(
                "human",
                &[crate::socket_peer_recovery::EVENT_RESYNC_CAPABILITY.to_owned()],
            )
            .unwrap();
        let unbind = core
            .presenters
            .bind(
                "session",
                "durable-socket-drain",
                std::sync::Arc::new(|| {}),
                &["human".to_owned()],
            )
            .unwrap();
        let authority = core.presenters.authority().unwrap().clone();
        let sender = core.presenters.connection("human").unwrap();
        let owner = authority.enroll(None);
        let registration = owner
            .register(
                unbind.binding(),
                &sender,
                crate::mcp_form_types::McpPresenterKind::Desktop,
            )
            .unwrap();
        owner.acquire(&registration).unwrap();
        // Overflow the 256-byte budget: the resync peer is shed and every later event is dropped until it resyncs.
        for _ in 0..8 {
            core.fanout
                .lock()
                .unwrap()
                .enqueue_control(
                    "human",
                    &serde_json::json!({"type":"notice","text":"backlog".repeat(10)}),
                )
                .unwrap();
        }
        let mut lines = tokio::io::BufReader::new(read).lines();
        let mut seen = Vec::<serde_json::Value>::new();
        async fn until(
            lines: &mut tokio::io::Lines<tokio::io::BufReader<tokio::io::DuplexStream>>,
            seen: &mut Vec<serde_json::Value>,
            kind: &str,
        ) -> serde_json::Value {
            loop {
                let line =
                    tokio::time::timeout(std::time::Duration::from_secs(5), lines.next_line())
                        .await
                        .unwrap()
                        .unwrap()
                        .unwrap();
                let value: serde_json::Value = serde_json::from_str(&line).unwrap();
                seen.push(value.clone());
                if value["type"] == kind {
                    return value;
                }
            }
        }
        let operation = maho_ai::utils::abort::AbortController::new();
        // This authority runs on the real clock: give the forms a deadline the test cannot reach.
        let form = || {
            let mut input = crate::mcp_form_dialog::tests::input(&operation);
            input.wire.deadline_ms = 60_000.;
            input
        };
        let presentation = unbind.binding().capture().unwrap();
        let first = tokio::spawn((presentation.present)(form()));
        let frame = until(&mut lines, &mut seen, "mcp_form_request").await;
        let mut envelope = serde_json::json!({});
        for key in [
            "sessionId",
            "bindingEpoch",
            "attachmentEpoch",
            "grantId",
            "dialogId",
            "dialogToken",
            "formRevision",
        ] {
            envelope[key] = frame[key].clone();
        }
        let mut presented = envelope.clone();
        presented["type"] = "mcp_form_presented".into();
        authority.receive(&sender, &presented);
        let mut response = envelope;
        response["type"] = "mcp_form_response".into();
        response["action"] = "accept".into();
        response["content"] = serde_json::json!({"value":"x"});
        authority.receive(&sender, &response);
        assert!(first.await.unwrap().human_admission());
        until(&mut lines, &mut seen, "mcp_form_resolved").await;
        let second = tokio::spawn((presentation.present)(form()));
        until(&mut lines, &mut seen, "mcp_form_request").await;
        unbind.binding().dispose();
        assert_eq!(
            second.await.unwrap().kind,
            crate::mcp_form_types::McpFormOutcomeKind::OperationAborted
        );
        let private: Vec<_> = seen
            .iter()
            .filter_map(|value| value["type"].as_str())
            .filter(|kind| kind.starts_with("mcp_form_"))
            .collect();
        assert_eq!(
            private,
            ["mcp_form_request", "mcp_form_resolved", "mcp_form_request"]
        );
        // The backlog was shed (at most the one notice already in flight reached the pipe).
        assert!(
            seen.iter()
                .filter(|value| value["type"] == "notice")
                .count()
                <= 1
        );
        let token = frame["dialogToken"].as_str().unwrap();
        assert!(
            failures
                .lock()
                .unwrap()
                .iter()
                .all(|error| !error.contains(token))
        );
        unbind.unbind(&core.presenters);
        core.unregister_connection("human");
    }
    /// A presenting connection enrolled on `session`, its grant live (`binding.capture()` is `Some`).
    fn presenting(
        core: &HostCore,
        id: &str,
        session: &str,
    ) -> crate::session_event_writer::McpFormsUnbind {
        let unbind = core
            .presenters
            .bind(
                session,
                &format!("durable-{id}-{session}"),
                std::sync::Arc::new(|| {}),
                &[id.to_owned()],
            )
            .unwrap();
        let authority = core.presenters.authority().unwrap().clone();
        let owner = authority.enroll(None);
        let registration = owner
            .register(
                unbind.binding(),
                &core.presenters.connection(id).unwrap(),
                crate::mcp_form_types::McpPresenterKind::Desktop,
            )
            .unwrap();
        owner.acquire(&registration).unwrap();
        assert!(unbind.binding().capture().is_some());
        unbind
    }
    fn enforcing_core() -> HostCore {
        let Some(core) = std::sync::Arc::try_unwrap(idle_core()).ok() else {
            panic!("the idle core is uniquely owned");
        };
        core.with_mcp_presenter_enforcement(true)
    }
    /// senpi `setConnectionCapabilities` withdraws on every capability write: the first attach records the
    /// connection's capabilities, and one without `interactive_user` + `mcp_form_presenter_v1` loses its grants.
    #[tokio::test]
    async fn attaching_without_presenter_capabilities_withdraws_the_connections_grants() {
        let core = enforcing_core();
        let _client = peer(&core, "c");
        let unbind = presenting(&core, "c", "rpc-1");
        {
            let mut registry = core.registry.lock().await;
            let mut profile = crate::session_registry::RpcSessionLaunchProfile::default();
            profile.runtime.cwd = "/tmp".to_owned();
            assert!(matches!(
                registry.admit_open(profile, false, 0.),
                Ok(crate::session_registry::OpenAdmission::Create(handle)) if handle == "rpc-1"
            ));
        }
        core.attach_connection(Some("c"), "rpc-1").await;
        assert!(
            core.fanout
                .lock()
                .unwrap()
                .targets
                .capabilities("c")
                .is_some()
        );
        assert!(unbind.binding().capture().is_none());
        unbind.unbind(&core.presenters);
    }
    /// senpi `onConnectionClosed`: a connection the fanout reaps after its queue failed is detached from the
    /// presenter authority, so its grant no longer presents.
    #[tokio::test]
    async fn a_connection_reaped_on_a_failed_queue_loses_its_presenter() {
        let core = enforcing_core();
        let (write, read) = tokio::io::duplex(64);
        drop(read);
        let (failed_tx, failed_rx) = tokio::sync::oneshot::channel();
        let failed_tx = std::sync::Mutex::new(Some(failed_tx));
        core.register_connection(
            "gone",
            crate::socket_event_fanout::SocketEventSinkActor::new(
                write,
                4096,
                30_000,
                std::sync::Arc::clone(&core.blocked),
                move |_| {
                    if let Some(failed) = failed_tx.lock().unwrap().take() {
                        let _ = failed.send(());
                    }
                },
            ),
        );
        let unbind = presenting(&core, "gone", "s");
        core.fanout
            .lock()
            .unwrap()
            .enqueue_control("gone", &serde_json::json!({"type":"notice"}))
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), failed_rx)
            .await
            .unwrap()
            .unwrap();
        core.broadcast_record(&serde_json::json!({"type":"host_notice"}));
        assert!(!core.fanout.lock().unwrap().is_registered("gone"));
        assert!(core.presenters.connection("gone").is_none());
        assert!(unbind.binding().capture().is_none());
        unbind.unbind(&core.presenters);
    }
    #[tokio::test]
    async fn protocol_info_names_the_hosts_tool_runtime_and_drops_a_negotiated_one() {
        let core = std::sync::Arc::try_unwrap(idle_core())
            .ok()
            .map(|core| core.with_tool_runtime(crate::host_capabilities::ToolRuntimeKind::Child));
        let Some(core) = core else {
            panic!("the idle core is uniquely owned");
        };
        let owned = core
            .advertised_capabilities()
            .into_iter()
            .filter(|capability| {
                crate::host_capabilities::is_host_owned_tool_runtime_capability(capability)
            })
            .collect::<Vec<_>>();
        assert_eq!(
            owned,
            crate::host_capabilities::tool_runtime_capabilities(
                crate::host_capabilities::ToolRuntimeKind::Child
            )
        );
    }
    #[tokio::test]
    async fn an_empty_host_drains_to_exit_once() {
        let core = idle_core();
        assert!(!core.drain_sweep().await);
        core.begin_drain().await;
        assert!(core.drain_sweep().await);
    }
    fn peer(core: &HostCore, id: &str) -> tokio::io::BufReader<tokio::io::DuplexStream> {
        let (write, read) = tokio::io::duplex(64 * 1024);
        core.register_connection(
            id,
            crate::socket_event_fanout::SocketEventSinkActor::new(
                write,
                4096,
                30_000,
                std::sync::Arc::clone(&core.blocked),
                |error| panic!("{error}"),
            ),
        );
        tokio::io::BufReader::new(read)
    }
    async fn next(reader: &mut tokio::io::BufReader<tokio::io::DuplexStream>) -> serde_json::Value {
        use tokio::io::AsyncBufReadExt;
        let mut line = String::new();
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            reader.read_line(&mut line),
        )
        .await
        .unwrap()
        .unwrap();
        serde_json::from_str(&line).unwrap()
    }
    /// A host whose in-process sessions run one faux turn held at its first chunk until `gate` opens; `disposed`
    /// fires when a runtime is torn down. An open naming a `sessionPath` persists the session at that path.
    fn gated_core(
        dir: &std::path::Path,
    ) -> (
        std::sync::Arc<HostCore>,
        tokio::sync::watch::Sender<bool>,
        tokio::sync::mpsc::UnboundedReceiver<()>,
        tokio::sync::mpsc::UnboundedReceiver<()>,
    ) {
        let (gate, gate_rx) = tokio::sync::watch::channel(false);
        let (calls_tx, calls) = tokio::sync::mpsc::unbounded_channel();
        let (disposed_tx, disposed) = tokio::sync::mpsc::unbounded_channel();
        let cwd = dir.to_string_lossy().into_owned();
        let factory_cwd = cwd.clone();
        let create_runtime: HostRuntimeFactory = std::sync::Arc::new(
            move |profile: crate::session_registry::RpcSessionLaunchProfile| {
                let cwd = factory_cwd.clone();
                let session_path = profile.session_path;
                let gate_rx = gate_rx.clone();
                let calls_tx = calls_tx.clone();
                let disposed_tx = disposed_tx.clone();
                Box::pin(async move {
                    use maho_ai::providers::faux::{
                        FauxAssistantMessageOptions, FauxTokenSize, RegisterFauxProviderOptions,
                        faux_assistant_message, faux_provider,
                    };
                    use maho_core::settings_manager::{InMemorySettingsStorage, SettingsManager};
                    let provider = faux_provider(RegisterFauxProviderOptions {
                        api: Some("faux".to_owned()),
                        token_size: Some(FauxTokenSize {
                            min: Some(1),
                            max: Some(1),
                        }),
                        scheduler_hook: Some(std::sync::Arc::new(move || {
                            let _ = calls_tx.send(());
                            let mut gate = gate_rx.clone();
                            Box::pin(async move {
                                while !*gate.borrow_and_update() {
                                    if gate.changed().await.is_err() {
                                        break;
                                    }
                                }
                            })
                        })),
                        ..Default::default()
                    })
                    .map_err(|error| error.to_string())?;
                    let model = provider
                        .get_model(Some("faux-1"))
                        .ok_or("the faux provider registers faux-1")?;
                    provider.set_responses(vec![
                        faux_assistant_message("done", FauxAssistantMessageOptions::default())
                            .into(),
                    ]);
                    let credentials =
                        maho_core::auth_storage::AuthStorage::in_memory(Default::default());
                    credentials.set(
                        &model.provider,
                        Some(serde_json::json!({"type":"api_key","key":"faux-test"})),
                    )?;
                    let dir = std::path::PathBuf::from(&cwd);
                    let mut models = maho_core::model_runtime::ModelRuntime::create_sync(
                        maho_core::model_runtime::CreateModelRuntimeOptions {
                            models_path: Some(dir.join("models.json")),
                            auth_path: Some(dir.join("auth.json")),
                            credentials: Some(std::sync::Arc::new(credentials)),
                            providers: Some(vec![provider.provider.clone()]),
                            ..Default::default()
                        },
                    )
                    .map_err(|error| error.to_string())?;
                    models.register_provider(
                        &model.provider,
                        maho_core::provider_composer::ProviderConfigInput {
                            config: maho_core::model_config_schema::ModelsJsonProvider {
                                api_key: Some("faux-test".into()),
                                ..Default::default()
                            },
                            ..Default::default()
                        },
                    )?;
                    let session = maho_core::sdk::create_agent_session(
                        maho_core::sdk::CreateAgentSessionOptions {
                            cwd: Some(cwd.clone()),
                            agent_dir: Some(cwd.clone()),
                            model_runtime: Some(models),
                            model: Some(model),
                            // A profile naming a path gets a persisted manager there, so the registry entry
                            // keeps its sessionPath (as the CLI factory does); otherwise the session stays in memory.
                            session_manager: Some(match session_path.as_deref() {
                                Some(path) => maho_core::session_manager::SessionManager::open(
                                    path,
                                    None,
                                    Some(cwd.as_str()),
                                    None,
                                )?,
                                None => maho_core::session_manager::SessionManager::in_memory(
                                    &cwd, None, None,
                                ),
                            }),
                            settings_manager: Some(SettingsManager::from_storage(
                                Box::new(InMemorySettingsStorage::default()),
                                false,
                            )),
                            no_tools: Some(maho_core::sdk::NoToolsMode::All),
                            auto_title_sessions: Some(false),
                            ..Default::default()
                        },
                    )
                    .await
                    .map_err(|error| error.to_string())?
                    .session;
                    let registry = session.model_registry().clone();
                    let services = maho_core::agent_session_services::AgentSessionServices {
                        cwd: cwd.clone(),
                        agent_dir: cwd,
                        auth_storage: std::sync::Arc::clone(&registry.auth_storage),
                        model_registry: registry,
                        settings_manager: SettingsManager::from_storage(
                            Box::new(InMemorySettingsStorage::default()),
                            false,
                        ),
                        diagnostics: vec![],
                    };
                    let mut runtime = maho_core::agent_session_runtime::AgentSessionRuntime::new(
                        session,
                        services,
                        vec![],
                        None,
                        None,
                    );
                    runtime.set_before_session_invalidate(Some(Box::new(move || {
                        let _ = disposed_tx.send(());
                    })));
                    Ok(runtime)
                })
                    as std::pin::Pin<
                        Box<
                            dyn std::future::Future<
                                    Output = Result<
                                        maho_core::agent_session_runtime::AgentSessionRuntime,
                                        String,
                                    >,
                                > + Send,
                        >,
                    >
            },
        );
        let writer = std::sync::Arc::new(crate::session_event_writer::SessionWriterActor::new(
            tokio::io::sink(),
        ));
        let core = std::sync::Arc::new(HostCore::new(
            HostCoreOptions {
                agent_dir: dir.to_path_buf(),
                cwd,
                create_runtime,
                capabilities: Vec::new(),
                close_grace_ms: 10_000,
            },
            writer,
        ));
        (core, gate, calls, disposed)
    }
    async fn reply_to(
        reader: &mut tokio::io::BufReader<tokio::io::DuplexStream>,
        id: &str,
    ) -> serde_json::Value {
        loop {
            let record = next(reader).await;
            if record["id"] == id {
                return record;
            }
        }
    }
    #[tokio::test]
    async fn a_deferred_release_attachment_survives_its_connection_dropping_mid_turn() {
        let temp = tempfile::tempdir().unwrap();
        let (core, gate, mut calls, mut disposed) = gated_core(temp.path());
        let mut client = peer(&core, "c");
        let cwd = temp.path().to_string_lossy().into_owned();
        core.handle(
            Some("c"),
            &serde_json::json!({"id":"o","type":"open_session","cwd":cwd}).to_string(),
        )
        .await
        .unwrap();
        let opened = reply_to(&mut client, "o").await;
        let session_id = opened["data"]["sessionId"].as_str().unwrap().to_owned();
        let prompt =
            serde_json::json!({"id":"p","type":"prompt","sessionId":session_id,"message":"hi"})
                .to_string();
        let prompting = std::sync::Arc::clone(&core);
        let turn = tokio::spawn(async move { prompting.handle(Some("c"), &prompt).await });
        tokio::time::timeout(std::time::Duration::from_secs(10), calls.recv())
            .await
            .expect("the turn reaches the provider")
            .expect("the provider stays alive");
        let in_flight = core.active_request_count(&session_id);
        let release =
            serde_json::json!({"id":"r","type":"release_attachment","sessionId":session_id})
                .to_string();
        // The command answers and completes without waiting for the turn: a disconnect can no longer cancel the
        // deferred release, and its request guard is gone before the wait.
        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            core.handle(Some("c"), &release),
        )
        .await
        .expect("release_attachment completes while the turn is still running")
        .unwrap();
        let reply = reply_to(&mut client, "r").await;
        assert_eq!(reply["data"]["deferred"], true);
        assert_eq!(core.active_request_count(&session_id), in_flight);
        assert!(
            core.has_session(&session_id).await,
            "the release waits for the turn"
        );
        gate.send(true).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(10), disposed.recv())
            .await
            .expect("the deferred release ends the session once its turn settles")
            .expect("the runtime reports its disposal");
        let _ = turn.await.unwrap();
    }
    // senpi session-router-routing.ts `unload_session`: the router answers through `unloadSession`; a session with
    // no file yet is refused (nothing to reopen it by) and stays open.
    #[tokio::test]
    async fn unload_session_is_routed_and_a_fileless_session_is_refused_and_kept() {
        let temp = tempfile::tempdir().unwrap();
        let (core, _gate, _calls, _disposed) = gated_core(temp.path());
        let mut client = peer(&core, "c");
        let cwd = temp.path().to_string_lossy().into_owned();
        core.handle(
            Some("c"),
            &serde_json::json!({"id":"o","type":"open_session","cwd":cwd}).to_string(),
        )
        .await
        .unwrap();
        let session_id = reply_to(&mut client, "o").await["data"]["sessionId"]
            .as_str()
            .unwrap()
            .to_owned();
        core.handle(
            Some("c"),
            &serde_json::json!({"id":"u","type":"unload_session","sessionId":session_id,"children":"maybe"})
                .to_string(),
        )
        .await
        .unwrap();
        let invalid = reply_to(&mut client, "u").await;
        assert_eq!(invalid["success"], false);
        assert_eq!(
            invalid["error"],
            "children must be \"refuse\" or \"detach\"."
        );
        core.handle(
            Some("c"),
            &serde_json::json!({"id":"u2","type":"unload_session","sessionId":session_id})
                .to_string(),
        )
        .await
        .unwrap();
        let refused = reply_to(&mut client, "u2").await;
        assert_eq!(refused["success"], true);
        assert_eq!(refused["data"]["outcome"], "refused");
        assert_eq!(refused["data"]["reason"], "unsupported");
        assert!(core.has_session(&session_id).await);
        assert!(
            core.advertised_capabilities()
                .iter()
                .any(|capability| capability == "unload_session")
        );
    }
    // senpi session-router-close.ts `tearDownUnloaded`: an idle session with a file unloads, its attached client
    // receives `session_parked { reason: "unloaded" }` naming the file, and the handle is gone.
    #[tokio::test]
    async fn an_idle_session_with_a_file_unloads_and_parks_as_unloaded() {
        let temp = tempfile::tempdir().unwrap();
        let (core, _gate, _calls, _disposed) = gated_core(temp.path());
        let mut client = peer(&core, "c");
        let cwd = temp.path().to_string_lossy().into_owned();
        let path = temp.path().join("s.jsonl").to_string_lossy().into_owned();
        core.handle(
            Some("c"),
            &serde_json::json!({"id":"o","type":"open_session","cwd":cwd,"sessionPath":path})
                .to_string(),
        )
        .await
        .unwrap();
        let session_id = reply_to(&mut client, "o").await["data"]["sessionId"]
            .as_str()
            .unwrap()
            .to_owned();
        core.handle(
            Some("c"),
            &serde_json::json!({"id":"u","type":"unload_session","sessionId":session_id})
                .to_string(),
        )
        .await
        .unwrap();
        let mut parked = None;
        let answer = loop {
            let record = next(&mut client).await;
            if record["type"] == "session_parked" {
                parked = Some(record);
            } else if record["id"] == "u" {
                break record;
            }
        };
        assert_eq!(answer["data"]["outcome"], "unloaded", "{answer}");
        assert_eq!(answer["data"]["sessionPath"], path.as_str());
        let parked = parked.expect("the park is sealed before the answer");
        assert_eq!(parked["reason"], "unloaded");
        assert_eq!(parked["sessionPath"], path.as_str());
        assert!(!core.has_session(&session_id).await);
    }
    #[test]
    fn open_child_sessions_count_rows_naming_the_session_as_parent_or_root() {
        let rows = [
            serde_json::json!({"durableSessionId":"p","context":{}}),
            serde_json::json!({"durableSessionId":"c1","context":{"parent_session_id":"p"}}),
            serde_json::json!({"durableSessionId":"c2","context":{"root_session_id":"p"}}),
            serde_json::json!({"durableSessionId":"x","context":{"parent_session_id":"q"}}),
        ];
        assert_eq!(open_child_session_count(&rows, "p"), 2);
    }
    // senpi-rpc-session-13 (`connectionLeft`): a connection that stops attaching ends the relocation request that
    // was asked of it, through the session binding.
    #[tokio::test]
    async fn a_dropped_connection_ends_the_relocation_request_it_was_asked() {
        let temp = tempfile::tempdir().unwrap();
        let (core, _gate, _calls, _disposed) = gated_core(temp.path());
        let mut client = peer(&core, "c");
        let cwd = temp.path().to_string_lossy().into_owned();
        core.handle(
            Some("c"),
            &serde_json::json!({"id":"o","type":"open_session","cwd":cwd}).to_string(),
        )
        .await
        .unwrap();
        let session_id = reply_to(&mut client, "o").await["data"]["sessionId"]
            .as_str()
            .unwrap()
            .to_owned();
        let binding = core
            .bindings
            .lock()
            .await
            .get(&session_id)
            .cloned()
            .unwrap();
        let relocation = binding.relocation().clone();
        let (asked_tx, mut asked_rx) = tokio::sync::oneshot::channel();
        let mut asking = Box::pin(relocation.targets(Some("c"), move |record| {
            let _ = asked_tx.send(record);
        }));
        assert!(futures::poll!(&mut asking).is_pending());
        let asked = asked_rx
            .try_recv()
            .expect("the request is sent to the connection");
        assert_eq!(asked["method"], "list_relocation_targets");
        core.release_connection("c").await;
        let answer = asking.await;
        assert_eq!(answer["reason"], "app_disconnected");
    }
    #[tokio::test]
    async fn tells_the_opener_its_queue_position_before_the_open_completes() {
        let temp = tempfile::tempdir().unwrap();
        let (core, _gate, _calls, _disposed) = gated_core(temp.path());
        let mut client = peer(&core, "c1");
        let cwd = temp.path().to_string_lossy().into_owned();
        core.handle(
            Some("c1"),
            &serde_json::json!({"id":"open-1","type":"open_session","cwd":cwd}).to_string(),
        )
        .await
        .unwrap();
        // The queued record precedes the open's reply and never wears the response-id field.
        let queued = loop {
            let record = next(&mut client).await;
            assert_ne!(
                record["id"], "open-1",
                "the queued record precedes the reply"
            );
            if record["type"] == "queued" {
                break record;
            }
        };
        assert_eq!(queued["type"], "queued");
        assert_eq!(queued["for_request"], "open-1");
        assert_eq!(queued["position"], 1.0);
        assert_eq!(queued["in_flight"], 0.0);
        assert!(queued.get("id").is_none());
        assert_eq!(reply_to(&mut client, "open-1").await["success"], true);
        assert!(core.opens_by_connection.lock().unwrap().is_empty());
    }
    // senpi `openWithBarrier`: an open already accepted anywhere on the host counts toward the position, and a
    // connection's release settles only after its opens did.
    #[tokio::test]
    async fn a_concurrent_open_counts_every_open_in_flight_and_release_waits_for_it() {
        let core = idle_core();
        let mut second = peer(&core, "c2");
        let first = core.open_barrier(Some("c1"), Some("open-1")).unwrap();
        let barrier = core.open_barrier(Some("c2"), Some("open-2")).unwrap();
        let queued = loop {
            let record = next(&mut second).await;
            if record["type"] == "queued" {
                break record;
            }
        };
        assert_eq!(
            (queued["position"].as_f64(), queued["in_flight"].as_f64()),
            (Some(2.0), Some(1.0))
        );
        // A stdio open has no connection and no barrier.
        assert!(core.open_barrier(None, Some("open-3")).is_none());
        let mut released = Box::pin(core.release_connection("c2"));
        assert!(
            futures::poll!(&mut released).is_pending(),
            "the release waits for the open in flight"
        );
        drop(barrier);
        released.await;
        assert!(!core.opens_by_connection.lock().unwrap().contains_key("c2"));
        drop(first);
        assert!(core.opens_by_connection.lock().unwrap().is_empty());
    }
    #[tokio::test]
    async fn reclaim_seals_session_closed_to_attached_connections_only() {
        let temp = tempfile::tempdir().unwrap();
        let (core, _gate, _calls, _disposed) = gated_core(temp.path());
        let mut owner = peer(&core, "a");
        let mut bystander = peer(&core, "b");
        let cwd = temp.path().to_string_lossy().into_owned();
        let path = temp.path().join("s.jsonl").to_string_lossy().into_owned();
        core.handle(
            Some("a"),
            &serde_json::json!({"id":"o","type":"open_session","cwd":cwd,"sessionPath":path})
                .to_string(),
        )
        .await
        .unwrap();
        let session_id = reply_to(&mut owner, "o").await["data"]["sessionId"]
            .as_str()
            .unwrap()
            .to_owned();
        let durable = core
            .live_identity(&session_id)
            .await
            .and_then(|(_, durable, _)| durable)
            .unwrap();
        core.handle(
            Some("a"),
            &serde_json::json!({"id":"k","type":"reclaim_session","sessionPath":path,"durableSessionId":durable,"expectedHostInstance":crate::protocol_identity::host_instance_id()})
                .to_string(),
        )
        .await
        .unwrap();
        let closed = loop {
            let record = next(&mut owner).await;
            if record["type"] == "session_closed" {
                break record;
            }
            assert_ne!(record["id"], "k", "the reclaim answered before its seal");
        };
        assert_eq!(closed["reason"], "client_close");
        assert_eq!(reply_to(&mut owner, "k").await["success"], true);
        // The unattached connection's next record is its own reply, not the reclaimed session's seal.
        core.handle(
            Some("b"),
            &serde_json::json!({"id":"m","type":"set_client_info","width":40}).to_string(),
        )
        .await
        .unwrap();
        loop {
            let record = next(&mut bystander).await;
            assert_ne!(record["type"], "session_closed");
            if record["id"] == "m" {
                break;
            }
        }
    }
    #[tokio::test]
    async fn open_session_merges_the_validated_spawned_by_into_the_launch_profile() {
        let (profile_tx, mut profiles) = tokio::sync::mpsc::unbounded_channel();
        let create_runtime: HostRuntimeFactory = std::sync::Arc::new(
            move |profile: crate::session_registry::RpcSessionLaunchProfile| {
                let _ = profile_tx.send(profile);
                Box::pin(async { Err("the profile is all this test needs".to_owned()) })
                    as std::pin::Pin<
                        Box<
                            dyn std::future::Future<
                                    Output = Result<
                                        maho_core::agent_session_runtime::AgentSessionRuntime,
                                        String,
                                    >,
                                > + Send,
                        >,
                    >
            },
        );
        let core = std::sync::Arc::new(HostCore::new(
            HostCoreOptions {
                agent_dir: std::path::PathBuf::from("/tmp"),
                cwd: "/tmp".to_owned(),
                create_runtime,
                capabilities: Vec::new(),
                close_grace_ms: 0,
            },
            std::sync::Arc::new(crate::session_event_writer::SessionWriterActor::new(
                tokio::io::sink(),
            )),
        ));
        let _client = peer(&core, "c");
        core.handle(
            Some("c"),
            &serde_json::json!({"id":"o","type":"open_session","cwd":"/tmp","spawnedBy":{"parentSessionId":"p","rootSessionId":"r","taskId":"t"}})
                .to_string(),
        )
        .await
        .unwrap();
        let profile = profiles
            .try_recv()
            .expect("the open reached the runtime factory");
        assert_eq!(
            profile.spawned_by,
            Some(maho_core::session_lifecycle_types::SpawnedBy {
                parent_session_id: "p".into(),
                root_session_id: "r".into(),
                task_id: "t".into(),
            })
        );
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn a_worker_reattach_to_a_sealed_session_is_refused_at_open() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("s.jsonl").to_string_lossy().into_owned();
        // A stand-in worker: answers prepare with the path and commit with an idle snapshot, then stays up.
        let script = format!(
            r#"while IFS= read -r line; do
  r=$(printf '%s' "$line" | sed -n 's/.*"request":\([0-9]*\).*/\1/p')
  case "$line" in
    *'"type":"prepare"'*) printf '{{"type":"prepared","request":%s,"sessionPath":"{path}"}}\n' "$r" ;;
    *'"type":"commit"'*) printf '{{"type":"ready","request":%s,"snapshot":{{"state":{{"sessionId":"d"}},"sessionPath":"{path}","liveSessionPaths":["{path}"],"busy":false,"streaming":false}}}}\n' "$r" ;;
    *'"request":'*) printf '{{"type":"result","request":%s}}\n' "$r" ;;
  esac
done"#
        );
        let spawn: crate::session_worker_client::SpawnSessionWorker =
            std::sync::Arc::new(move || {
                tokio::process::Command::new("sh")
                    .arg("-c")
                    .arg(&script)
                    .stdin(std::process::Stdio::piped())
                    .stdout(std::process::Stdio::piped())
                    .kill_on_drop(true)
                    .spawn()
            });
        let core = std::sync::Arc::try_unwrap(idle_core())
            .ok()
            .expect("the idle core is uniquely owned")
            .with_worker_registry(crate::worker_session_registry::WorkerSessionRegistry::new(
                serde_json::json!({}),
                spawn,
                0,
            ));
        let core = std::sync::Arc::new(core);
        let mut client = peer(&core, "c");
        let open = serde_json::json!({"id":"o","type":"open_session","cwd":temp.path(),"sessionPath":path})
            .to_string();
        core.handle(Some("c"), &open).await.unwrap();
        let opened = reply_to(&mut client, "o").await;
        let session_id = opened["data"]["sessionId"].as_str().unwrap().to_owned();
        let reserved = core
            .default_writer
            .reserve_close_response(&session_id, &serde_json::json!({}), true)
            .unwrap()
            .unwrap();
        core.default_writer
            .complete_close_response(reserved, &session_id, serde_json::json!({}), true, None)
            .unwrap();
        let reopen = serde_json::json!({"id":"o2","type":"open_session","cwd":temp.path(),"sessionPath":path})
            .to_string();
        core.handle(Some("c"), &reopen).await.unwrap();
        let refused = reply_to(&mut client, "o2").await;
        assert_eq!(refused["errorCode"], "session_output_overflow");
        assert_eq!(refused["errorData"]["action"], "unload_and_reopen");
        let workers = core.workers.clone().unwrap();
        assert_eq!(
            workers.with_entry(&session_id, |entry| entry.close.attachments),
            Some(1)
        );
    }
    /// dialog-observer.test.ts (senpi#4266): a `dialogs: true` observer that is not attached receives the session's
    /// dialog feed and its own snapshot marker, never another connection's marker; the owner keeps its content.
    #[tokio::test]
    async fn a_dialog_observer_receives_the_dialog_feed_without_attaching() {
        let core = idle_core();
        let mut owner = peer(&core, "owner");
        let mut observer = peer(&core, "observer");
        core.fanout
            .lock()
            .unwrap()
            .attach("owner", "s", 0.)
            .unwrap();
        {
            let fanout = core.fanout.lock().unwrap();
            fanout.status.lock().unwrap().source.upsert(
                "s",
                crate::session_status_source::entry_view(
                    crate::session_status_source::RegistryEntryFields {
                        state: crate::session_registry::RpcSessionState::Open,
                        kind: maho_ext_api::SessionKind::Interactive,
                        cwd: "/w",
                        attachments: 1,
                        durable_session_id: Some("d1".into()),
                        session_path: None,
                        context: &std::collections::BTreeMap::new(),
                        overlay: None,
                    },
                ),
                crate::session_status_source::StatusRuntime::None,
            );
        }
        core.handle(
            Some("observer"),
            r#"{"id":"sub","type":"subscribe_sessions","mirror":"all","dialogs":true,"observe":true}"#,
        )
        .await
        .unwrap();
        let answer = next(&mut observer).await;
        assert_eq!(answer["command"], "subscribe_sessions");
        assert_eq!(answer["data"]["snapshot"][0]["sessionId"], "s");
        assert_eq!(answer["data"]["snapshot"][0]["seq"], 0);
        route_session_record(
            &core.fanout,
            &core.default_writer,
            "s",
            Some("owner"),
            serde_json::json!({"type":"snapshot_marker","markerId":"owner-marker"}),
        );
        route_session_record(
            &core.fanout,
            &core.default_writer,
            "s",
            Some("observer"),
            serde_json::json!({"type":"snapshot_marker","markerId":"m1"}),
        );
        route_session_record(
            &core.fanout,
            &core.default_writer,
            "s",
            None,
            serde_json::json!({"type":"ui_request_pending","uiRequestId":"q1","method":"confirm","request":{"method":"confirm"}}),
        );
        route_session_record(
            &core.fanout,
            &core.default_writer,
            "s",
            None,
            serde_json::json!({"type":"agent_end","messages":[]}),
        );
        let marker = next(&mut observer).await;
        assert_eq!(
            (marker["type"].as_str(), marker["markerId"].as_str()),
            (Some("snapshot_marker"), Some("m1"))
        );
        let raised = next(&mut observer).await;
        assert_eq!(raised["type"], "ui_request_pending");
        assert_eq!(raised["sessionId"], "s");
        // Mirrored content, then the status delta it caused, both after the dialog record.
        assert_eq!(next(&mut observer).await["type"], "agent_end");
        let status = next(&mut observer).await;
        assert_eq!(
            (status["type"].as_str(), status["seq"].as_u64()),
            (Some("session_status"), Some(1))
        );
        // The owner never sees observation records, only the content it is attached to.
        assert_eq!(next(&mut owner).await["type"], "agent_end");
        assert!(!core.attachments.lock().unwrap().owns(Some("observer"), "s"));
    }
    #[tokio::test]
    async fn a_dropped_connection_gives_up_every_attachment_it_held() {
        let core = idle_core();
        core.attachments.lock().unwrap().attach("gone", "s");
        core.release_connection("gone").await;
        assert!(!core.attachments.lock().unwrap().owns(Some("gone"), "s"));
    }
    #[tokio::test]
    async fn attachment_commands_refuse_without_a_live_identity_or_an_attachment() {
        let core = idle_core();
        let mut client = peer(&core, "c");
        core.handle(
            Some("c"),
            &serde_json::json!({"id":"a","type":"attach_session","sessionId":"rpc-9","durableSessionId":"d"})
                .to_string(),
        )
        .await
        .unwrap();
        let reply = next(&mut client).await;
        assert_eq!(reply["error"], "unknown_session");
        assert_eq!(reply["errorCode"], "unknown_session");
        core.handle(
            Some("c"),
            &serde_json::json!({"id":"r","type":"release_attachment","sessionId":"rpc-9"})
                .to_string(),
        )
        .await
        .unwrap();
        let reply = next(&mut client).await;
        assert_eq!(reply["error"], "session_not_attached");
        assert_eq!(reply["errorData"]["reason"], "not_attached");
        core.handle(
            Some("c"),
            &serde_json::json!({"id":"k","type":"reclaim_session","sessionPath":"","durableSessionId":"d","expectedHostInstance":"h"})
                .to_string(),
        )
        .await
        .unwrap();
        assert_eq!(next(&mut client).await["error"], "unknown_session");
        core.begin_drain().await;
        core.handle(
            Some("c"),
            &serde_json::json!({"id":"a2","type":"attach_session","sessionId":"rpc-9","durableSessionId":"d"})
                .to_string(),
        )
        .await
        .unwrap();
        let reply = next(&mut client).await;
        assert_eq!(reply["error"], "session_busy_lifecycle");
        assert_eq!(reply["errorData"]["reason"], "host_draining");
    }
    #[tokio::test]
    async fn the_idle_policy_reports_an_empty_host_once_its_window_elapses() {
        let core = idle_core();
        let (_stop, stopped) = tokio::sync::watch::channel(false);
        assert!(
            core.run_idle_policy(
                HostIdlePolicy {
                    idle_eviction_ms: f64::INFINITY,
                    empty_exit_ms: 100.
                },
                || true,
                stopped
            )
            .await
        );
    }
    #[tokio::test]
    async fn every_attached_connection_receives_session_records_and_responses_stay_with_their_requester()
     {
        let core = idle_core();
        let mut first = peer(&core, "one");
        let mut second = peer(&core, "two");
        {
            let mut fanout = core.fanout.lock().unwrap();
            fanout.attach("one", "s", 0.).unwrap();
            fanout.attach("two", "s", 0.).unwrap();
        }
        route_session_record(
            &core.fanout,
            &core.default_writer,
            "s",
            Some("one"),
            serde_json::json!({"type":"agent_end"}),
        );
        route_session_record(
            &core.fanout,
            &core.default_writer,
            "s",
            Some("two"),
            serde_json::json!({"type":"response","command":"prompt"}),
        );
        assert_eq!(next(&mut first).await["type"], "agent_end");
        assert_eq!(next(&mut second).await["type"], "agent_end");
        let response = next(&mut second).await;
        assert_eq!(response["command"], "prompt");
        assert_eq!(response["sessionId"], "s");
    }
    #[tokio::test]
    async fn socket_lifecycle_records_and_host_replies_carry_observed_at() {
        let core = idle_core();
        let mut client = peer(&core, "c");
        core.fanout.lock().unwrap().attach("c", "s", 0.).unwrap();
        route_session_record(
            &core.fanout,
            &core.default_writer,
            "s",
            None,
            serde_json::json!({"type":"session_lifecycle","lifecycle":{"seq":1}}),
        );
        assert!(next(&mut client).await["lifecycle"]["observedAt"].is_string());
        core.respond_record(
            Some("c"),
            "s",
            serde_json::json!({"type":"response","command":"open_session","success":true,"data":{"sessionId":"s","state":{"lifecycle":{"seq":1}}}}),
        );
        assert!(next(&mut client).await["data"]["state"]["lifecycle"]["observedAt"].is_string());
    }
    #[tokio::test]
    async fn a_snapshot_marker_is_never_broadcast_to_attachments() {
        let core = idle_core();
        let mut client = peer(&core, "c");
        core.fanout.lock().unwrap().attach("c", "s", 0.).unwrap();
        route_session_record(
            &core.fanout,
            &core.default_writer,
            "s",
            Some("c"),
            serde_json::json!({"type":"snapshot_marker","markerId":"m"}),
        );
        route_session_record(
            &core.fanout,
            &core.default_writer,
            "s",
            None,
            serde_json::json!({"type":"agent_end"}),
        );
        assert_eq!(next(&mut client).await["type"], "agent_end");
    }
    #[tokio::test]
    async fn late_joiner_replays_the_in_flight_message() {
        let core = idle_core();
        let _first = peer(&core, "one");
        let mut late = peer(&core, "late");
        core.fanout.lock().unwrap().attach("one", "s", 0.).unwrap();
        route_session_record(
            &core.fanout,
            &core.default_writer,
            "s",
            None,
            serde_json::json!({"type":"message_start"}),
        );
        core.fanout.lock().unwrap().attach("late", "s", 0.).unwrap();
        assert_eq!(next(&mut late).await["type"], "message_start");
    }
    #[tokio::test]
    async fn set_client_info_registers_connection_capabilities() {
        let core = idle_core();
        let mut client = peer(&core, "c");
        core.handle(Some("c"),&serde_json::json!({"type":"set_client_info","width":40,"capabilities":["media_placeholders"]}).to_string()).await.unwrap();
        assert_eq!(next(&mut client).await["command"], "set_client_info");
        assert_eq!(
            core.fanout.lock().unwrap().targets.capabilities("c"),
            Some(&["media_placeholders".to_owned()][..])
        );
    }
}
/// The supervisor-lifetime and empty-exit triggers one shared host reacts to (senpi
/// `runSocketHost`'s watchdog binding and its `canExitWhenEmpty` window).
#[derive(Debug, Default)]
pub struct HostShutdownSignals {
    pub watchdog: Option<crate::host_watchdog::HostWatchdogConfig>,
    pub empty_exit_ms: Option<f64>,
}
/// The shutdown triggers a supervised host reads from its own environment: the canonical (or
/// branded) watchdog binding the supervisor set, and the empty-exit window from the idle policy.
pub fn host_shutdown_signals_from_env() -> HostShutdownSignals {
    let env: HashMap<String, String> = std::env::vars().collect();
    let policy = resolve_host_idle_policy(&env, None, None);
    HostShutdownSignals {
        watchdog: crate::host_internal_transport::host_watchdog_binding(),
        empty_exit_ms: Some(policy.empty_exit_ms),
    }
}
/// Process cleanup the embedding binary runs on SIGTERM/SIGHUP before the host exits (senpi
/// `registerShutdownSignals` calls `killTrackedDetachedChildren`, which lives in the CLI crate).
static SIGNAL_CLEANUP: std::sync::OnceLock<fn()> = std::sync::OnceLock::new();
pub fn set_host_signal_cleanup(cleanup: fn()) {
    let _ = SIGNAL_CLEANUP.set(cleanup);
}
/// Serve the shared host: stdio when no socket was named, otherwise the public Unix socket
/// (senpi `runMultiSessionHost`).
pub async fn run_multi_session_host(
    core: std::sync::Arc<HostCore>,
    listen: Option<String>,
) -> std::io::Result<()> {
    run_multi_session_host_with_signals(core, listen, host_shutdown_signals_from_env()).await
}
/// Serve the shared host with explicit shutdown triggers: the seam the CLI entry and its tests use.
pub async fn run_multi_session_host_with_signals(
    core: std::sync::Arc<HostCore>,
    listen: Option<String>,
    signals: HostShutdownSignals,
) -> std::io::Result<()> {
    #[cfg(not(any(unix, windows)))]
    {
        let _ = signals;
        match listen.as_deref() {
            None | Some("stdio://") => run_stdio_host(core).await,
            Some(_) => Err(std::io::Error::other(
                "rpc host sockets are not supported on this platform yet",
            )),
        }
    }
    #[cfg(any(unix, windows))]
    {
        match listen.as_deref() {
            None | Some("stdio://") => {
                // A stdio host serves no socket: it publishes claims only into a told daemon directory.
                core.install_endpoint_reservations(None).await;
                run_stdio_host(core).await
            }
            Some(path) => run_socket_host(core, path.to_owned(), signals).await,
        }
    }
}
#[cfg(not(unix))]
async fn run_stdio_host(core: std::sync::Arc<HostCore>) -> std::io::Result<()> {
    use tokio::io::AsyncReadExt;
    let mut input = tokio::io::stdin();
    let mut reader = crate::jsonl::JsonlLineReader::new(crate::jsonl::MAX_RPC_LINE_CHARACTERS)
        .map_err(std::io::Error::other)?;
    let mut chunk = [0u8; 8192];
    loop {
        let count = input.read(&mut chunk).await?;
        let records = if count == 0 {
            reader.finish()
        } else {
            reader.push(&chunk[..count])
        };
        for record in records {
            let crate::jsonl::LineRecord::Line(line) = record else {
                continue;
            };
            core.handle(None, &line)
                .await
                .map_err(std::io::Error::other)?;
        }
        if count == 0 {
            core.default_writer
                .flush()
                .await
                .map_err(std::io::Error::other)?;
            return Ok(());
        }
    }
}
#[cfg(unix)]
async fn run_stdio_host(core: std::sync::Arc<HostCore>) -> std::io::Result<()> {
    use tokio::io::AsyncReadExt;
    let mut input = tokio::io::stdin();
    let mut reader = crate::jsonl::JsonlLineReader::new(crate::jsonl::MAX_RPC_LINE_CHARACTERS)
        .map_err(std::io::Error::other)?;
    let mut chunk = [0u8; 8192];
    let mut commands = Vec::new();
    // An empty host (no session ever opened, or all closed) must not stay resident forever: the
    // idle policy evicts idle sessions and ends the host once the empty window elapses (senpi
    // `runStdioHost`'s `onEmptyExit` + `startHostObservers`).
    let env: HashMap<String, String> = std::env::vars().collect();
    let (stop, stopped) = tokio::sync::watch::channel(false);
    let observers = core.start_host_observers(stopped.clone());
    let idle_core = core.clone();
    let policy = resolve_host_idle_policy(&env, None, None);
    let mut empty_exit =
        tokio::spawn(async move { idle_core.run_idle_policy(policy, || true, stopped).await });
    let mut empty_exit_done = false;
    let result = loop {
        let count = tokio::select! {
            result=poll_host_commands(&mut commands),if !commands.is_empty()=>{if let Err(error)=result{break Err(std::io::Error::other(error));}continue;},
            exit=&mut empty_exit,if !empty_exit_done=>{empty_exit_done=true;if matches!(exit,Ok(true)){break core.default_writer.flush().await.map_err(std::io::Error::other);}continue;},
            count=input.read(&mut chunk)=>match count{Ok(count)=>count,Err(error)=>break Err(error)},
        };
        let records = if count == 0 {
            reader.finish()
        } else {
            reader.push(&chunk[..count])
        };
        for record in records {
            let crate::jsonl::LineRecord::Line(line) = record else {
                continue;
            };
            let core = core.clone();
            commands.push(Box::pin(async move { core.handle(None, &line).await }) as HostCommand);
        }
        if count == 0 {
            drop(commands);
            break core
                .default_writer
                .flush()
                .await
                .map_err(std::io::Error::other);
        }
    };
    let _ = stop.send(true);
    observers.abort();
    empty_exit.abort();
    result
}
#[cfg(any(unix, windows))]
type HostCommand = std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send>>;
#[cfg(any(unix, windows))]
async fn poll_host_commands(commands: &mut Vec<HostCommand>) -> Result<(), String> {
    let (index, result) = std::future::poll_fn(|cx| {
        for (index, command) in commands.iter_mut().enumerate() {
            if let std::task::Poll::Ready(result) = command.as_mut().poll(cx) {
                return std::task::Poll::Ready((index, result));
            }
        }
        std::task::Poll::Pending
    })
    .await;
    drop(commands.remove(index));
    result
}
/// The bound host endpoint: a Unix socket, or the first instance of a Windows named pipe.
#[cfg(unix)]
struct HostListener(tokio::net::UnixListener);
#[cfg(unix)]
type HostStream = tokio::net::UnixStream;
#[cfg(unix)]
impl HostListener {
    fn bind(path: &str, _secret: Option<&[u8]>) -> std::io::Result<Self> {
        use std::os::unix::fs::PermissionsExt;
        let listener = tokio::net::UnixListener::bind(path)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
        Ok(Self(listener))
    }
    async fn accept(&mut self) -> std::io::Result<HostStream> {
        Ok(self.0.accept().await?.0)
    }
}
#[cfg(windows)]
struct HostListener {
    address: String,
    server: tokio::net::windows::named_pipe::NamedPipeServer,
}
#[cfg(windows)]
type HostStream = tokio::net::windows::named_pipe::NamedPipeServer;
#[cfg(windows)]
impl HostListener {
    /// Listens on the pipe the logical path resolves to (senpi `listen` + `resolveSocketTransportAddress`).
    fn bind(path: &str, secret: Option<&[u8]>) -> std::io::Result<Self> {
        let address =
            crate::socket_transport::resolve_socket_transport_address(path, "win32", secret)
                .map_err(std::io::Error::other)?;
        let server = crate::host_lifecycle::win32_pipe::create_first_instance(&address)?;
        Ok(Self { address, server })
    }
    async fn accept(&mut self) -> std::io::Result<HostStream> {
        self.server.connect().await?;
        let next = tokio::net::windows::named_pipe::ServerOptions::new()
            .reject_remote_clients(true)
            .create(&self.address)?;
        Ok(std::mem::replace(&mut self.server, next))
    }
}
/// The process signals that end a socket host: SIGTERM/SIGHUP on unix (exit codes 143/129), the console
/// break/close/shutdown events on Windows, where senpi registers SIGTERM only (exit code 143).
#[cfg(unix)]
struct HostShutdownSignal {
    terminate: tokio::signal::unix::Signal,
    hangup: tokio::signal::unix::Signal,
}
#[cfg(unix)]
impl HostShutdownSignal {
    fn new() -> std::io::Result<Self> {
        Ok(Self {
            terminate: tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?,
            hangup: tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())?,
        })
    }
    async fn recv(&mut self) -> i32 {
        tokio::select! {
            _=self.terminate.recv()=>143,
            _=self.hangup.recv()=>129,
        }
    }
}
#[cfg(windows)]
struct HostShutdownSignal {
    brk: tokio::signal::windows::CtrlBreak,
    close: tokio::signal::windows::CtrlClose,
    shutdown: tokio::signal::windows::CtrlShutdown,
}
#[cfg(windows)]
impl HostShutdownSignal {
    fn new() -> std::io::Result<Self> {
        Ok(Self {
            brk: tokio::signal::windows::ctrl_break()?,
            close: tokio::signal::windows::ctrl_close()?,
            shutdown: tokio::signal::windows::ctrl_shutdown()?,
        })
    }
    async fn recv(&mut self) -> i32 {
        tokio::select! {
            _=self.brk.recv()=>143,
            _=self.close.recv()=>143,
            _=self.shutdown.recv()=>143,
        }
    }
}
/// Whether a `--listen` path is a Windows named-pipe address (no directory to create, no secret file of its own).
#[cfg(windows)]
fn is_pipe_address(path: &str) -> bool {
    path.to_lowercase().starts_with("\\\\.\\pipe\\")
}
/// senpi `resolveSocketPath` for the empty `unix://` form: the default `<agentDir>/rpc/rpc.sock`.
#[cfg(windows)]
fn resolve_listen_path(value: &str, agent_dir: &std::path::Path) -> String {
    let path = crate::host_ensure::normalize_socket_path(value);
    if path.is_empty() {
        agent_dir
            .join("rpc")
            .join("rpc.sock")
            .to_string_lossy()
            .into_owned()
    } else {
        path.to_owned()
    }
}
#[cfg(any(unix, windows))]
async fn run_socket_host(
    core: std::sync::Arc<HostCore>,
    listen: String,
    signals: HostShutdownSignals,
) -> std::io::Result<()> {
    #[cfg(unix)]
    let path = crate::host_ensure::normalize_socket_path(&listen).to_owned();
    #[cfg(windows)]
    let path = resolve_listen_path(&listen, &core.options.agent_dir);
    #[cfg(unix)]
    let secret: Option<std::sync::Arc<Vec<u8>>> = None;
    // On Windows the secret handshake is the only boundary: the supervisor hands the child its internal secret
    // file; a bare host derives one beside its logical path (senpi `ensureSocketSecret`).
    #[cfg(windows)]
    let secret: Option<std::sync::Arc<Vec<u8>>> = {
        let secret_file = match std::env::var(crate::socket_transport::SOCKET_SECRET_FILE_ENV) {
            Ok(file) if !file.is_empty() => file,
            _ => {
                crate::socket_transport::socket_secret_path(&path).map_err(std::io::Error::other)?
            }
        };
        Some(std::sync::Arc::new(
            crate::socket_transport::ensure_socket_secret(std::path::Path::new(&secret_file))?
                .to_vec(),
        ))
    };
    #[cfg(windows)]
    let pipe_address = is_pipe_address(&path);
    #[cfg(unix)]
    let pipe_address = false;
    if !pipe_address && let Some(parent) = std::path::Path::new(&path).parent() {
        crate::host_daemon_paths::create_private_directory(parent)?;
    }
    let HostShutdownSignals {
        watchdog,
        empty_exit_ms,
    } = signals;
    // senpi `internalHostTransport`: the sealed hop (stdin key), the Windows secret, or the public key filter.
    let transport = std::sync::Arc::new(crate::host_internal_transport::internal_host_transport(
        watchdog,
        secret.clone(),
        (!pipe_address).then_some((core.options.agent_dir.as_path(), path.as_str())),
    )?);
    let watchdog = transport.watchdog_config.clone();
    let handoff =
        std::sync::Arc::new(crate::host_handoff_announcement::HandoffAnnouncement::default());
    let public_socket = watchdog
        .as_ref()
        .and_then(|config| config.public_socket.clone());
    // The public entry's ownership token lives in the supervisor's private directory, which the
    // watchdog cleanup removes - so it is read BEFORE that cleanup (senpi `beforeCleanup`).
    let public_owner: std::sync::Arc<
        std::sync::Mutex<Option<crate::socket_ownership::SocketFileIdentity>>,
    > = std::sync::Arc::new(std::sync::Mutex::new(None));
    let owner_file = watchdog
        .as_ref()
        .and_then(|config| config.scratch_dir.as_ref())
        .map(|dir| dir.join(crate::socket_ownership::PUBLIC_SOCKET_IDENTITY_FILE));
    let capture_slot = public_owner.clone();
    let capture = async move {
        if let Some(path) = owner_file
            && let Ok(Some(identity)) = crate::socket_ownership::read_socket_identity_file(&path)
        {
            *capture_slot
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(identity);
        }
        Ok(())
    };
    let callback_owner = public_owner.clone();
    let (reason_tx, reason_rx) = tokio::sync::watch::channel::<Option<String>>(None);
    // An absent watchdog is inert, not a shutdown signal when its task returns.
    let _reason_sender = reason_tx.clone();
    // A socket host installs the SIGUSR1 drain below, so it advertises `generation_handoff` and can
    // be handed off to a newer generation instead of being killed (senpi multi-session-host.ts:268):
    // the capability IS the proof a handler exists, and a host without it is never signalled.
    let handoff_public_socket = public_socket.clone();
    let (drain_tx, drain_rx) = tokio::sync::watch::channel(false);
    let _drain_sender = drain_tx.clone();
    #[cfg(unix)]
    {
        // Register the handler FIRST; only a successful registration may advertise the capability.
        let mut user1 =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::user_defined1())?;
        core.enable_handoff();
        let drain_tx = drain_tx.clone();
        tokio::spawn(async move {
            loop {
                if user1.recv().await.is_none() {
                    break;
                }
                if drain_tx.send(true).is_err() {
                    break;
                }
            }
        });
    }
    // The supervisor's signal-free relay, the drain channel every platform shares (senpi#3653).
    let _drain_request_watch = crate::host_stop_intent::host_child_drain_request_file(
        &std::env::vars().collect(),
    )
    .map(|file| {
        let relay = drain_tx.clone();
        crate::host_stop_intent::watch_drain_request(
            file,
            move |_request| {
                let _ = relay.send(true);
            },
            None,
        )
    });
    // Armed BEFORE the listen: a supervisor death during the listen transition must still end this
    // host and clean its private endpoint (senpi multi-session-host.ts:470-490).
    let watchdog_task = tokio::spawn(crate::host_watchdog::arm_host_watchdog(
        watchdog,
        capture,
        move |reason| {
            eprintln!("senpi rpc host: {reason}; shutting down");
            if let Some(path) = public_socket.as_deref() {
                let identity = *callback_owner
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                crate::socket_ownership::unlink_owned_socket(
                    path,
                    identity,
                    if cfg!(windows) {
                        "win32"
                    } else {
                        std::env::consts::OS
                    },
                    |_| {},
                );
            }
            let _ = reason_tx.send(Some(reason));
        },
    ));
    // senpi `runSocketHost`: the successor reopens what its predecessor parked, from the daemon directory of the
    // endpoint clients dial (the supervisor's public socket when supervised).
    let endpoint_socket = handoff_public_socket
        .clone()
        .unwrap_or_else(|| path.clone());
    let daemon_dir = crate::host_daemon_paths::create_host_daemon_paths(
        &endpoint_socket,
        &core.options.agent_dir,
    )
    .dir;
    core.set_handover_parked_dir(daemon_dir.clone());
    core.install_endpoint_reservations(Some(&endpoint_socket))
        .await;
    // senpi `runSocketHost`: the runtime this host loaded, computed once at startup. A successor an idle handover
    // started for another runtime exits before it listens, leaving its predecessor serving.
    let runtime_build_id = startup_runtime_build_id(&core.options.cwd);
    if let Ok(expected) = std::env::var(crate::host_idle_handover::EXPECTED_RUNTIME_BUILD_ID_ENV)
        && Some(expected.as_str()) != runtime_build_id.as_deref()
    {
        eprintln!(
            "senpi rpc host: runtime {} is not the handover target {expected}; exiting",
            runtime_build_id.as_deref().unwrap_or("unreadable")
        );
        std::process::exit(1);
    }
    if let Some(runtime_build_id) = &runtime_build_id {
        core.set_runtime_build_id(runtime_build_id.clone());
    }
    #[cfg(unix)]
    if let Some(runtime_build_id) = runtime_build_id {
        core.enable_idle_handover(
            crate::host_handover_wire::HostEndpoint {
                agent_dir: core.options.agent_dir.clone(),
                socket: endpoint_socket.clone(),
            },
            runtime_build_id,
        );
    }
    {
        let core = core.clone();
        let host_socket = endpoint_socket.clone();
        tokio::spawn(async move {
            let store = crate::host_handover_parked::FileHandoverParkedStore::new(daemon_dir);
            let opener_core = core.clone();
            let opener = move |entry: crate::host_handover_parked::HandoverParkedEntry| {
                let core = opener_core.clone();
                Box::pin(async move { core.reopen_parked(entry).await })
                    as crate::host_handover_reopen::BoxFuture<
                        'static,
                        Result<(), crate::host_handover_reopen::ReopenError>,
                    >
            };
            let still_serving = || !core.is_draining();
            let holder = crate::host_handover_reopen::ReopenHolder {
                host_instance: crate::protocol_identity::host_instance_id().to_owned(),
                host_socket: Some(host_socket),
                should_reopen: Some(&still_serving),
            };
            crate::host_handover_reopen::reopen_handover_parked_sessions(
                &store,
                &opener,
                Some(&holder),
            )
            .await;
        });
    }
    let mut listener = HostListener::bind(&path, secret.as_deref().map(Vec::as_slice))?;
    let bound = crate::socket_ownership::stat_socket_identity(std::path::Path::new(&path))?;
    // A key record that cannot be published safely ends this host: held connections are refused, never served
    // keyless (senpi `transport.publish(() => shutdown(1))`).
    if transport.publish().is_err() {
        watchdog_task.abort();
        crate::socket_ownership::unlink_owned_socket(
            &path,
            bound,
            if cfg!(windows) {
                "win32"
            } else {
                std::env::consts::OS
            },
            |_| {},
        );
        transport.dispose();
        std::process::exit(1);
    }
    let draining_flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    #[cfg(unix)]
    let presenter_dir = core.start_presenter_control().await;
    let connections = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let mut next = 0u64;
    let mut draining = false;
    let mut empty_since: Option<tokio::time::Instant> = None;
    let mut reason_rx = reason_rx;
    let mut shutdown_signal = HostShutdownSignal::new()?;
    let mut signal_code = None;
    let mut drain_rx = drain_rx;
    let mut tick = std::time::Duration::from_millis(250);
    // Self-observation, idle eviction and the child reaper run for the host's life (senpi
    // `startHostObservers`, the router's idle sweep, `startHostChildReaper`). This loop keeps
    // the connection-gated empty exit, so the sweep here only evicts.
    let env: HashMap<String, String> = std::env::vars().collect();
    let (observers_stop, observers_stopped) = tokio::sync::watch::channel(false);
    let observers = core.start_host_observers(observers_stopped.clone());
    let idle_policy = resolve_host_idle_policy(&env, None, Some(f64::INFINITY));
    let idle_core = core.clone();
    let idle = tokio::spawn(async move {
        idle_core
            .run_idle_policy(idle_policy, || false, observers_stopped)
            .await;
    });
    let reaper =
        crate::child_reaper::start_host_child_reaper(&env, std::env::consts::OS, |message| {
            eprintln!("senpi rpc host: {message}")
        });
    loop {
        tokio::select! {
            biased;
            changed=reason_rx.changed()=>{if changed.is_err()||reason_rx.borrow().is_some(){break;}},
            changed=drain_rx.changed()=>{
                if changed.is_ok()&&*drain_rx.borrow()&&!draining{
                    draining=true;
                    tick=std::time::Duration::from_millis(crate::session_command_router::DRAIN_SWEEP_MS);
                    eprintln!("senpi rpc host: draining for a generation handoff");
                    let superseded=endpoint_superseded(&path,&bound,handoff_public_socket.as_deref(),&public_owner);
                    let announced_socket=handoff_public_socket.clone().unwrap_or_else(||path.clone());
                    let generation=crate::protocol_identity::host_generation(&std::env::vars().collect::<std::collections::HashMap<String,String>>());
                    // The announcement is registered before the drain is visible to admitting connections.
                    let announcing=handoff.begin(async{Ok(superseded)},crate::protocol_identity::host_instance_id(),generation,&announced_socket,|message|eprintln!("senpi rpc host: {message}"));
                    draining_flag.store(true,std::sync::atomic::Ordering::SeqCst);
                    // A failed announcement still drains: parking is what lets this generation exit (senpi#2285).
                    if let Some(value)=announcing.await{core.broadcast_record(&value);}
                    core.begin_drain().await;
                }
            },
            code=shutdown_signal.recv()=>{if let Some(cleanup)=SIGNAL_CLEANUP.get(){cleanup();}signal_code=Some(code);break;},
            accepted=listener.accept()=>{
                let socket=accepted?;
                if draining{drop(socket);continue;}
                // The sealed hop authenticates before the host registers a connection: one that reached the listener
                // before the drain began still gets the announcement, then a clean close (senpi `deliverLate`).
                let arrived_before_drain=true;
                next+=1;let connection=format!("socket-{next}");
                connections.fetch_add(1,std::sync::atomic::Ordering::SeqCst);
                empty_since=None;
                let core=core.clone();let counter=connections.clone();let transport=transport.clone();let handoff=handoff.clone();let draining_flag=draining_flag.clone();
                tokio::spawn(async move{
                    if let Some(stream)=transport.admit(socket).await{
                        if draining_flag.load(std::sync::atomic::Ordering::SeqCst){
                            let _=handoff.deliver_late(stream,arrived_before_drain);
                        }else{
                            serve_socket_connection(core,connection,stream).await;
                        }
                    }
                    counter.fetch_sub(1,std::sync::atomic::Ordering::SeqCst);
                });
            }
            ()=tokio::time::sleep(tick)=>{
                if draining&&core.drain_sweep().await{break;}
                let empty=(draining||connections.load(std::sync::atomic::Ordering::SeqCst)==0)&&core.session_count().await==0;
                match empty_exit_ms.filter(|window|window.is_finite()){
                    Some(window) if empty=>{let since=*empty_since.get_or_insert(tokio::time::Instant::now());if since.elapsed().as_secs_f64()*1000.>=window{break;}},
                    _=>empty_since=None,
                }
            }
        }
    }
    watchdog_task.abort();
    let _ = observers_stop.send(true);
    observers.abort();
    idle.abort();
    core.dispose_handover();
    if let Some(reaper) = reaper {
        reaper.stop();
    }
    core.dispose_presenter_control();
    // senpi `await handoverAnswered()`: a handover that completed at once drains this host; its caller still gets the
    // answer.
    core.handover_answered().await;
    #[cfg(unix)]
    drop(presenter_dir);
    // Ownership-checked: only the entry THIS process bound is removed (senpi `unlinkOwnedSocket`).
    crate::socket_ownership::unlink_owned_socket(
        &path,
        bound,
        if cfg!(windows) {
            "win32"
        } else {
            std::env::consts::OS
        },
        |_| {},
    );
    transport.dispose();
    if let Some(code) = signal_code {
        std::process::exit(code);
    }
    Ok(())
}
/// senpi `startupRuntimeBuildId`: this process's runtime id over its launch profile, or `None` (logged) when unreadable.
#[cfg(any(unix, windows))]
fn startup_runtime_build_id(cwd: &str) -> Option<String> {
    let argv = std::env::args().skip(1).collect::<Vec<_>>();
    let computed = crate::protocol_identity::host_launch_profile(&argv, cwd)
        .map_err(std::io::Error::other)
        .and_then(|profile| {
            crate::runtime_build_id::compute_runtime_build_id(
                &crate::runtime_build_id::RuntimeBuildIdInput::from_profile(&profile.core),
            )
        });
    match computed {
        Ok(id) => Some(id),
        Err(cause) => {
            eprintln!("senpi rpc host: runtime identity unreadable: {cause}");
            None
        }
    }
}
/// Whether the endpoint this host serves is held by another socket entry now (senpi
/// `runSocketHost`'s `endpointSuperseded`): a supervised host answers for the PUBLIC path its
/// supervisor bound, a bare host for the path it bound itself.
#[cfg(any(unix, windows))]
fn endpoint_superseded(
    own_path: &str,
    own: &Option<crate::socket_ownership::SocketFileIdentity>,
    public_socket: Option<&str>,
    public_owner: &std::sync::Mutex<Option<crate::socket_ownership::SocketFileIdentity>>,
) -> bool {
    let platform = if cfg!(windows) {
        "win32"
    } else {
        std::env::consts::OS
    };
    match public_socket {
        Some(path) => {
            let identity = *public_owner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            crate::socket_ownership::socket_entry_replaced(path, identity, platform)
        }
        None => crate::socket_ownership::socket_entry_replaced(own_path, *own, platform),
    }
}
/// One accepted connection's lifetime: register its writer, pump its commands, release it.
#[cfg(any(unix, windows))]
async fn serve_socket_connection(
    core: std::sync::Arc<HostCore>,
    connection: String,
    socket: crate::host_lifecycle_proxy::BoxedHop,
) {
    // Admission (sealed hop, secret or public key filter) already ran in `InternalHostTransport::admit`.
    let (read, write) = tokio::io::split(socket);
    let failed = connection.clone();
    let actor = crate::socket_event_fanout::SocketEventSinkActor::new(
        write,
        crate::socket_event_fanout::DEFAULT_QUEUE_BYTES,
        crate::socket_event_fanout::DEFAULT_STALL_MS,
        std::sync::Arc::clone(&core.blocked),
        move |error| eprintln!("senpi rpc connection {failed} cut: {error}"),
    );
    core.register_connection(&connection, actor);
    let mut lines = tokio::io::BufReader::new(read);
    use tokio::io::AsyncBufReadExt;
    let mut buffer = String::new();
    let mut commands = Vec::new();
    loop {
        buffer.clear();
        let read = tokio::select! {
            result=poll_host_commands(&mut commands),if !commands.is_empty()=>{if result.is_err(){break;}continue;},
            read=lines.read_line(&mut buffer)=>read,
        };
        match read {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let line = buffer
            .trim_end_matches('\n')
            .trim_end_matches('\r')
            .to_owned();
        let core = core.clone();
        let tag = connection.clone();
        commands.push(Box::pin(async move { core.handle(Some(&tag), &line).await }) as HostCommand);
    }
    drop(commands);
    core.release_retained_attachments(&connection).await;
    core.unregister_connection(&connection);
    // A socket that dies without close_session still owns its sessions' attachments and path
    // reservations (senpi `detach` -> `router.releaseConnection`).
    core.release_connection(&connection).await;
}
#[cfg(test)]
mod tests {
    use super::*;
    /// senpi `sessionMemory`: only the session's own kernels count, and a session without one reports zeros.
    #[test]
    fn a_rows_memory_sums_only_its_own_kernels() {
        let kernel = |session: &str, bytes| {
            maho_core::memory_report::kernel_registry_read::HostKernelMemoryRow {
                session_id: session.into(),
                language: "js".into(),
                live_bytes: bytes,
                measure: "heap".into(),
            }
        };
        let kernels = [kernel("a", 10), kernel("b", 5), kernel("a", 7)];
        assert_eq!(
            session_memory(&kernels, "a"),
            serde_json::json!({"main_heap_bytes":0,"kernel_heap_bytes":17,"kernel_count":2})
        );
        assert_eq!(
            session_memory(&kernels, "c"),
            serde_json::json!({"main_heap_bytes":0,"kernel_heap_bytes":0,"kernel_count":0})
        );
    }
    /// senpi `createZeroSessionTrim`: a drop to zero is answered by one `host_trimmed` record on the next tick;
    /// with no collector the record says `collected: false`.
    #[test]
    fn a_drop_to_zero_reports_one_uncollected_trim() {
        let records = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(1));
        let (seen, sessions) = (
            std::sync::Arc::clone(&records),
            std::sync::Arc::clone(&count),
        );
        let mut trimmer = create_zero_session_trim(
            move |record| seen.lock().unwrap().push(record),
            move || sessions.load(std::sync::atomic::Ordering::SeqCst),
        );
        trimmer.start();
        trimmer.observe(1);
        count.store(0, std::sync::atomic::Ordering::SeqCst);
        trimmer.observe(0);
        trimmer.check();
        trimmer.check();
        let records = records.lock().unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0]["type"], "host_trimmed");
        assert_eq!(records[0]["collected"], false);
        assert_eq!(records[0]["sessions"], 0);
    }
    #[test]
    fn explicit_policy_overrides_environment_and_bad_env_uses_defaults() {
        let env = HashMap::from([
            (RPC_SESSION_IDLE_EVICTION_MS_ENV.into(), "100".into()),
            (RPC_HOST_EMPTY_EXIT_MS_ENV.into(), "invalid".into()),
        ]);
        assert_eq!(
            resolve_host_idle_policy(&env, None, None),
            HostIdlePolicy {
                idle_eviction_ms: 100.,
                empty_exit_ms: DEFAULT_HOST_EMPTY_EXIT_MS
            }
        );
        assert_eq!(
            resolve_host_idle_policy(&env, Some(f64::INFINITY), Some(0.)),
            HostIdlePolicy {
                idle_eviction_ms: f64::INFINITY,
                empty_exit_ms: 0.
            }
        );
    }
}
