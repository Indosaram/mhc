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
}
/// A refused open: the wire message, and the registry code when one carried it (`session_path_in_use`).
struct OpenRefusal {
    message: String,
    code: Option<String>,
}
impl OpenRefusal {
    fn plain(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            code: None,
        }
    }
    fn from_registry(error: &crate::session_registry::RpcSessionRegistryError) -> Self {
        Self {
            message: error.to_string(),
            code: Some(error.code.clone()),
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
        Self {
            registry: tokio::sync::Mutex::new(
                crate::session_registry::RpcSessionRegistry::default(),
            ),
            default_writer,
            attachments: Default::default(),
            fanout: Default::default(),
            blocked: Default::default(),
            widths: Default::default(),
            activity: Default::default(),
            memory_pressure: std::sync::atomic::AtomicBool::new(false),
            pending_capabilities: Default::default(),
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
        }
    }
    /// Names the daemon directory the handoff-parked record lives under (senpi `handoverParkedDir`); the first
    /// call wins.
    pub fn set_handover_parked_dir(&self, dir: std::path::PathBuf) {
        let _ = self.handover_parked_dir.set(dir);
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
    }
    /// Unregisters a connection's sink, its capabilities and its render widths.
    pub fn unregister_connection(&self, connection: &str) {
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
        // senpi `hostToolRuntimeCapabilities`: the host's own placement first; a negotiated `tool_runtime:*` never overrides it.
        let tool_runtime = crate::host_capabilities::host_tool_runtime_capabilities(
            self.tool_runtime,
            &self.options.capabilities,
        );
        for capability in [
            "multi_session",
            AUTO_TITLE_SESSIONS_CAPABILITY,
            MEDIA_PLACEHOLDERS_CAPABILITY,
            crate::rpc_types::STEER_WHEN_IDLE_CAPABILITY,
            crate::rpc_types::QUEUED_MESSAGE_REVISION_CAPABILITY,
            crate::rpc_types::USER_ACTIVITY_CAPABILITY,
            SESSION_HELD_CAPABILITY,
            RESUME_SESSION_CAPABILITY,
            ATTENDED_CAPABILITY,
            RETAIN_ON_DISCONNECT_CAPABILITY,
            SESSION_CONTEXT_CAPABILITY,
            SESSION_KIND_CAPABILITY,
            AUTO_TITLE_PER_SESSION_CAPABILITY,
            DURABLE_SESSION_ID_CAPABILITY,
        ]
        .into_iter()
        .chain(std::iter::once(
            crate::socket_peer_recovery::EVENT_RESYNC_CAPABILITY,
        ))
        .chain(tool_runtime.iter().map(String::as_str))
        {
            if !capabilities.iter().any(|existing| existing == capability) {
                capabilities.push(capability.to_owned());
            }
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
    /// Drops every per-connection record of a handle the host ended (senpi `forgetSessionOwnership`).
    fn forget_session(&self, session_id: &str) {
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
    fn respond_record(&self, connection: Option<&str>, session_id: &str, value: serde_json::Value) {
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
    /// Feed one inbound JSONL line on one connection (senpi's `handle` inside `createHostCore`).
    pub async fn handle(&self, connection: Option<&str>, line: &str) -> Result<(), String> {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            self.error(
                connection,
                None,
                "parse",
                format!(
                    "Failed to parse command: {}",
                    crate::connection_handler::json_parse_error_message(line)
                ),
                None,
            );
            return Ok(());
        };
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
        // senpi router `handle`: every routed request counts against its session until it answers, so a
        // release can tell other work from itself.
        let _request = self.begin_request(command.session_id.as_deref());
        match &command.body {
            crate::rpc_types::RpcCommandBody::GetProtocolInfo => {
                // senpi `process.argv.slice(2)`: every CLI argument after the executable.
                let argv = std::env::args().skip(1).collect::<Vec<_>>();
                let mut data =
                    crate::protocol_identity::host_launch_profile(&argv, &self.options.cwd)
                        .ok()
                        .map_or_else(
                            || serde_json::json!({}),
                            |profile| {
                                crate::protocol_identity::protocol_identity(
                                    profile,
                                    &std::env::vars().collect(),
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
            crate::rpc_types::RpcCommandBody::ListSessions { include_workers } => {
                let sessions = self.list_rows(include_workers.unwrap_or(false)).await;
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
                            self.release_owner_attachment(connection, &session_id);
                        }
                        // Only the LAST attachment tears the session down; a surviving attachment keeps
                        // the shared binding and its event stream (senpi `close`/`claimClose`).
                        if claim.finalizer != Some(true) {
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
                        .await?
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
        let admitted = {
            let mut registry = self.registry.lock().await;
            registry.admit_open(profile.clone(), retain, crate::connection_handler::now_ms())
        };
        match admitted {
            Err(error) => Err(OpenRefusal::from_registry(&error)),
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
                Ok(serde_json::json!({"sessionId":handle,"state":state,"attached":true}))
            }
            Ok(crate::session_registry::OpenAdmission::Create(handle)) => {
                let created = {
                    let mut registry = self.registry.lock().await;
                    registry
                        .create_admitted_runtime(&handle, (self.options.create_runtime)(profile))
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
            Ok(context) => self.success(
                connection,
                id,
                "set_session_context",
                Some(serde_json::json!({"context": context})),
                Some(session_id),
            ),
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
            Ok(crate::worker_session_registry::WorkerOpen::Attached(handle)) => (handle, true),
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
        {
            let mut fanout = self
                .fanout
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            fanout.set_session_kind(&handle, kind);
            if let Some(connection) = connection {
                if fanout.targets.capabilities(connection).is_none()
                    && let Err(error) = fanout.set_capabilities(connection, &capabilities)
                {
                    eprintln!("senpi rpc connection {connection} failed: {error}");
                }
                if let Err(error) =
                    fanout.attach(connection, &handle, crate::connection_handler::now_ms())
                {
                    eprintln!("senpi rpc connection {connection} failed: {error}");
                }
            }
        }
        if let Some(connection) = connection {
            self.attachments
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .attach(connection, &handle);
        }
        worker.set_connection_roles(std::sync::Arc::clone(&self.connection_roles));
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
                self.release_owner_attachment(connection, &handle);
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
            self.release_owner_attachment(connection, &session_id);
        }
        if claim.finalizer != Some(true) {
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
        let (runtime, kind) = {
            let registry = self.registry.lock().await;
            match registry.get(session_id) {
                Some(entry) => (
                    entry.runtime.as_ref().map(std::sync::Arc::clone),
                    entry.identity.kind,
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
        {
            let mut fanout = self
                .fanout
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            fanout.set_session_kind(session_id, kind);
            if let Some(connection) = connection {
                if fanout.targets.capabilities(connection).is_none()
                    && let Err(error) = fanout.set_capabilities(connection, &capabilities)
                {
                    eprintln!("senpi rpc connection {connection} failed: {error}");
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
        self.attachments
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .attach(connection, session_id);
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
        let sink: std::sync::Arc<dyn Fn(serde_json::Value) + Send + Sync> =
            std::sync::Arc::new(move |value: serde_json::Value| {
                let owner = CURRENT_CONNECTION.try_with(Clone::clone).ok().flatten();
                route_session_record(&fanout, &writer, &tag, owner.as_deref(), value);
            });
        let binding = std::sync::Arc::new(
            crate::session_binding::create_rpc_session_binding(
                session_id.to_owned(),
                runtime.session().clone(),
                sink,
                capabilities,
                Some(std::sync::Arc::clone(&self.connection_roles)),
            )
            .await,
        );
        bindings.insert(session_id.to_owned(), binding);
    }
    /// Releases every session a dropped connection still owned (senpi `releaseConnection`). Each
    /// attachment goes through the refcounted detach a close uses, so a session another connection
    /// still holds keeps running, a retained one stays open at zero attachments, and only the last
    /// owner tears it down. A turn in flight runs to settlement first; a drain owns teardown itself.
    pub async fn release_connection(self: &std::sync::Arc<Self>, connection: &str) {
        let owned = self
            .attachments
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take(connection);
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
                        let mut settled = worker.settled();
                        let core = std::sync::Arc::clone(self);
                        let session_id = session_id.clone();
                        tokio::spawn(async move {
                            while worker.busy() && !worker.is_stopped() {
                                if settled.changed().await.is_err() {
                                    break;
                                }
                            }
                            core.release_owned_session(&session_id).await;
                        });
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
                    let session_id = session_id.clone();
                    tokio::spawn(async move {
                        let _ = settled_rx.await;
                        drop(subscription);
                        core.release_owned_session(&session_id).await;
                    });
                    continue;
                }
                self.release_owned_session(&session_id).await;
            }
        }
    }
    /// One owned handle's refcounted detach (senpi `releaseOwnedSession`): tolerant of a lost race,
    /// and only the LAST attachment disposes the binding and closes the runtime.
    async fn release_owned_session(&self, session_id: &str) {
        if let Some(workers) = &self.workers {
            let Ok(claim) = workers.mark_close(session_id, true) else {
                return;
            };
            if claim.finalizer != Some(true) {
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
        if claim.finalizer != Some(true) {
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
            run_host_monitors(
                &env,
                &core.activity,
                &core.blocked,
                memory,
                crate::connection_handler::now_ms,
                publish,
                stopped,
            )
            .await;
        })
    }
    /// Detaches the closing connection's UI/width state (senpi `releaseOwnerAttachment`); the
    /// ownership count itself was released by the close claim.
    fn release_owner_attachment(&self, owner: &str, session_id: &str) {
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
    fn prepare_outgoing_state(
        &self,
        session_path: &str,
        move_id: &str,
    ) -> Result<
        crate::session_release::StagedState,
        crate::session_release_interrupt::ReleaseWriteError,
    > {
        crate::session_release::prepare_outgoing_state(session_path, move_id)
            .map_err(crate::session_release_interrupt::ReleaseWriteError::Other)
    }
    fn discard_outgoing_state(&self, dir: &str) {
        crate::session_release::discard_outgoing_state(dir);
    }
    async fn tear_down(&self, session_id: &str, session_path: &str) -> bool {
        self.core.tear_down_released(session_id, session_path).await
    }
}
tokio::task_local! {pub(crate) static CURRENT_CONNECTION:Option<String>;}
/// Routes one session record (senpi `SessionEventWriter.enqueue`): responses, bash updates and
/// blocking UI requests go to their requester only; content goes to the connections attached to
/// the session and is remembered for late joiners; lifecycle records follow their visibility
/// rule. With no registered connection the record takes the shared stdio lane.
fn route_session_record(
    fanout: &std::sync::Mutex<crate::session_event_fanout::SessionEventFanout>,
    writer: &crate::session_event_writer::SessionWriterActor,
    session_id: &str,
    owner: Option<&str>,
    record: serde_json::Value,
) {
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
    #[tokio::test]
    async fn a_dropped_connection_gives_up_every_attachment_it_held() {
        let core = idle_core();
        core.attachments.lock().unwrap().attach("gone", "s");
        core.release_connection("gone").await;
        assert!(!core.attachments.lock().unwrap().owns(Some("gone"), "s"));
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
        watchdog: crate::host_watchdog::read_host_watchdog_config_from_brand_env(&env),
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
            None | Some("stdio://") => run_stdio_host(core).await,
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
                    let successor=superseded.then(||crate::rpc_types::RpcHostSuccessor{socket:handoff_public_socket.clone().unwrap_or_else(||path.clone())});
                    if let Ok(value)=serde_json::to_value(crate::rpc_types::RpcHostLifecycleEvent::HostSuperseded{instance_id:crate::protocol_identity::host_instance_id().to_owned(),generation:crate::protocol_identity::host_generation(&std::env::vars().collect::<std::collections::HashMap<String,String>>()),successor}){core.broadcast_record(&value);}
                    core.begin_drain().await;
                }
            },
            code=shutdown_signal.recv()=>{if let Some(cleanup)=SIGNAL_CLEANUP.get(){cleanup();}signal_code=Some(code);break;},
            accepted=listener.accept()=>{
                let socket=accepted?;
                if draining{drop(socket);continue;}
                next+=1;let connection=format!("socket-{next}");
                connections.fetch_add(1,std::sync::atomic::Ordering::SeqCst);
                empty_since=None;
                let core=core.clone();let counter=connections.clone();let secret=secret.clone();
                tokio::spawn(async move{serve_socket_connection(core,connection,socket,secret).await;counter.fetch_sub(1,std::sync::atomic::Ordering::SeqCst);});
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
    if let Some(reaper) = reaper {
        reaper.stop();
    }
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
    if let Some(code) = signal_code {
        std::process::exit(code);
    }
    Ok(())
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
    mut socket: HostStream,
    secret: Option<std::sync::Arc<Vec<u8>>>,
) {
    // A wrong or missing secret ends the connection before any JSONL byte is read or answered.
    if let Some(secret) = secret
        && crate::socket_transport::authenticate_socket(&mut socket, &secret)
            .await
            .is_err()
    {
        return;
    }
    #[cfg(unix)]
    let (read, write) = socket.into_split();
    #[cfg(windows)]
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
