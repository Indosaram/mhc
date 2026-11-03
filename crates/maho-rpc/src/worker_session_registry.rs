use crate::session_registry::{
    RpcSessionLaunchProfile, RpcSessionRegistryError, SessionCloseState, SessionIdentity,
};
use crate::session_worker_client::{
    SessionWorkerCallbacks, SessionWorkerClient, SpawnSessionWorker,
};
use crate::{
    session_path_reservations::{LiveWorkerPaths, SessionPathReservations, SessionWriteGrant},
    session_registry::RpcSessionState,
};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};
/// The routes one worker's output and lifecycle take back into the host (senpi reaches them
/// through the bound \`SessionEventWriter\` and the router's \`close\`).
pub struct WorkerRoutes {
    pub output: Box<dyn Fn(serde_json::Value, Option<String>) + Send + Sync>,
    pub request_close: Box<dyn Fn() + Send + Sync>,
    /// A worker failure the bound clients see as \`session_error\` (senpi \`fail\`).
    pub failure: Box<dyn Fn(&str) + Send + Sync>,
    /// The worker died with a failure: the terminal close record (senpi \`publishTerminalFailure\`).
    pub terminal_failure: Box<dyn Fn(&str) + Send + Sync>,
    /// The writer's security bridge: which connections can present a grant, and where a settled dialog is recorded
    /// (senpi \`SessionEventWriter\`'s grant methods). \`None\` forwards a worker's grant traffic unchanged.
    pub security: Option<std::sync::Arc<dyn crate::session_worker_client::WorkerSecurityWriter>>,
    /// The host writer's presenter state when private MCP forms are enforced (senpi `writer.mcpPresenters`).
    pub presenters: Option<std::sync::Arc<crate::session_event_writer::SessionMcpPresenters>>,
}
pub struct WorkerSessionEntry {
    pub close: SessionCloseState,
    pub profile: RpcSessionLaunchProfile,
    pub identity: SessionIdentity,
    pub worker: Arc<SessionWorkerClient>,
    pub durable_session_id: Option<String>,
    pub session_path: Option<String>,
    pub cwd: String,
    pub last_command_at: f64,
    pub requested_path_key: Option<String>,
    /// Patches `set_session_context` applied over the frozen open-time context (senpi `contextOverlay`).
    pub context_overlay: Option<crate::session_context_patch::SessionContextOverlay>,
    failure: Option<String>,
}
#[derive(Default)]
struct RegistryState {
    entries: std::collections::BTreeMap<String, WorkerSessionEntry>,
    /// senpi `onSizeChange` (the in-process registry's hook, kept by the worker registry too).
    on_size_change: Option<crate::session_registry::SizeChangeHook>,
    ownership: WorkerOwnership,
    serial: u64,
}
pub enum WorkerOpen {
    Created(String),
    Attached(String),
}
/// Transport-side lifecycle owner for worker-runtime sessions (senpi \`WorkerSessionRegistry\`):
/// every session runs in its own worker process, and this registry owns admission, attachments
/// and the session-path reservations the workers ask for before they write.
pub struct WorkerSessionRegistry {
    state: Arc<Mutex<RegistryState>>,
    configuration: serde_json::Value,
    spawn: SpawnSessionWorker,
    pub close_grace_ms: u64,
}
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
/// senpi \`assertValidSessionId\` as a check (the native assertion panics).
fn valid_session_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    !bytes.is_empty()
        && bytes[0].is_ascii_alphanumeric()
        && bytes[bytes.len() - 1].is_ascii_alphanumeric()
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}
fn set_state(
    state: &mut RegistryState,
    handle: &str,
    next: crate::session_registry::RpcSessionState,
) {
    if let Some(entry) = state.entries.get_mut(handle) {
        entry.close.state = next;
    }
    state.ownership.states.insert(handle.to_owned(), next);
}
impl RegistryState {
    fn reserve(&mut self, handle: &str, path: &str) -> SessionWriteGrant {
        let live = self
            .entries
            .get(handle)
            .filter(|entry| entry.close.state == RpcSessionState::Open)
            .and_then(|entry| entry.worker.snapshot());
        let live = live.as_ref().map(|snapshot| {
            (
                snapshot.live_session_paths.clone(),
                snapshot.session_path.clone(),
            )
        });
        let view = live.as_ref().map(|(paths, session_path)| LiveWorkerPaths {
            live_paths: paths,
            session_path: session_path.as_deref(),
        });
        self.ownership.reserve(handle, path, view.as_ref())
    }
    /// Only compare spellings already tied to a granted identity; caller paths are never inspected here.
    fn known_reservation_key(&self, path: &str) -> Option<String> {
        if self.ownership.reservations.owner(path).is_some() {
            return Some(path.to_owned());
        }
        for entry in self.entries.values() {
            if entry.profile.session_path.as_deref() == Some(path)
                && let Some(key) = &entry.requested_path_key
                && self.ownership.reservations.owner(key).is_some()
            {
                return Some(key.clone());
            }
            if let Some(snapshot) = entry.worker.snapshot()
                && snapshot.state["sessionFile"].as_str() == Some(path)
            {
                return snapshot.session_path;
            }
        }
        None
    }
    fn attach(
        &mut self,
        owner: &str,
        path: &str,
        retain: bool,
        now: f64,
    ) -> Result<WorkerOpen, RpcSessionRegistryError> {
        let entry = self
            .entries
            .get_mut(owner)
            .filter(|entry| {
                entry.close.state == RpcSessionState::Open
                    && entry.worker.binding_ready()
                    && entry
                        .worker
                        .snapshot()
                        .and_then(|snapshot| snapshot.session_path)
                        .as_deref()
                        == Some(path)
            })
            .ok_or_else(|| RpcSessionRegistryError::new("session_path_in_use", None, None))?;
        entry.close.attachments += 1;
        // Retention is a property of the live session: any attach may ask for it, none may revoke it.
        entry.close.retain_on_disconnect |= retain;
        entry.last_command_at = now;
        Ok(WorkerOpen::Attached(owner.to_owned()))
    }
}
impl WorkerSessionRegistry {
    pub fn new(
        configuration: serde_json::Value,
        spawn: SpawnSessionWorker,
        close_grace_ms: u64,
    ) -> Self {
        Self {
            state: Default::default(),
            configuration,
            spawn,
            close_grace_ms,
        }
    }
    pub fn size(&self) -> usize {
        lock(&self.state).entries.len()
    }
    /// senpi `onSizeChange`: told the entry count after every open admits and every worker exit removes one.
    pub fn set_on_size_change(&self, hook: crate::session_registry::SizeChangeHook) {
        lock(&self.state).on_size_change = Some(hook);
    }
    /// Memory admission is the in-process registry's concern: a worker session runs in its own
    /// process and this registry already bounds occupancy with \`too_many_sessions\`.
    pub fn set_worker_admission(&self, _refusal: Option<(u64, u64)>) {}
    pub fn reservation_count(&self, handle: &str) -> usize {
        lock(&self.state).ownership.reservations.count(handle)
    }
    pub fn with_entry<R>(
        &self,
        handle: &str,
        read: impl FnOnce(&mut WorkerSessionEntry) -> R,
    ) -> Option<R> {
        lock(&self.state).entries.get_mut(handle).map(read)
    }
    pub fn worker(&self, handle: &str) -> Option<Arc<SessionWorkerClient>> {
        lock(&self.state)
            .entries
            .get(handle)
            .map(|entry| Arc::clone(&entry.worker))
    }
    /// senpi \`getForCommand\`: closing sessions accept only the control commands.
    pub fn get_for_command(
        &self,
        handle: &str,
        command: &str,
        now: f64,
    ) -> Result<Arc<SessionWorkerClient>, RpcSessionRegistryError> {
        let mut state = lock(&self.state);
        let entry = state
            .entries
            .get_mut(handle)
            .ok_or_else(|| RpcSessionRegistryError::new("unknown_session", None, None))?;
        if entry.close.state == RpcSessionState::Quarantined
            || (entry.close.state == RpcSessionState::Closing
                && !matches!(
                    command,
                    "abort" | "abort_bash" | "extension_ui_response" | "extension_ui_progress"
                ))
        {
            return Err(RpcSessionRegistryError::new("session_closing", None, None));
        }
        if !matches!(
            entry.close.state,
            RpcSessionState::Open | RpcSessionState::Closing
        ) {
            return Err(RpcSessionRegistryError::new("unknown_session", None, None));
        }
        // Polling a session nobody holds is observation, not work that needs its runtime; an attached
        // client's polling keeps its session alive exactly as before.
        if entry.close.attachments > 0
            || crate::session_command_activity::refreshes_session_activity(command)
        {
            entry.last_command_at = now;
        }
        Ok(Arc::clone(&entry.worker))
    }
    /// senpi `attachSession` (`attachLiveSession`): a synchronous refcount change on an open entry; never opens a
    /// path and never applies a launch profile. `true` when the attach ended a retained session's parked window.
    pub fn attach_session(&self, handle: &str, now: f64) -> Result<bool, RpcSessionRegistryError> {
        let mut state = lock(&self.state);
        let entry = state
            .entries
            .get_mut(handle)
            .filter(|entry| entry.close.state == RpcSessionState::Open)
            .ok_or_else(|| RpcSessionRegistryError::new("unknown_session", None, None))?;
        let was_parked = entry.close.retain_on_disconnect && entry.close.attachments == 0;
        entry.close.attachments += 1;
        entry.last_command_at = now;
        Ok(was_parked)
    }
    /// senpi `attach` (the profile half): an attach that names another surface or browser engine moves the live
    /// session to it, and a named permission preset is sent even when the record already names it (senpi#2842).
    ///
    /// NATIVE-DIVERGENCE(senpi awaits the setters before `attachments++`): the native attach takes the attachment
    /// under the registry lock and applies the profile after it, since the worker requests await.
    pub async fn apply_attach_profile(
        &self,
        handle: &str,
        requested: &RpcSessionLaunchProfile,
    ) -> Result<(), String> {
        let (worker, surface, engine, preset) = {
            let mut state = lock(&self.state);
            let Some(entry) = state.entries.get_mut(handle) else {
                return Ok(());
            };
            let surface = requested
                .prompt_surface
                .clone()
                .filter(|surface| entry.profile.prompt_surface.as_ref() != Some(surface));
            let engine = requested
                .browser_engine
                .clone()
                .filter(|engine| entry.profile.browser_engine.as_ref() != Some(engine));
            let preset = requested.runtime.permission_preset.clone();
            if let Some(surface) = &surface {
                entry.profile.prompt_surface = Some(surface.clone());
            }
            if let Some(engine) = &engine {
                entry.profile.browser_engine = Some(engine.clone());
            }
            if let Some(preset) = &preset {
                entry.profile.runtime.permission_preset = Some(preset.clone());
            }
            (Arc::clone(&entry.worker), surface, engine, preset)
        };
        if let Some(surface) = surface {
            worker.set_prompt_surface(&surface).await?;
        }
        if let Some(engine) = engine {
            worker.set_browser_engine(&engine).await?;
        }
        if let Some(preset) = preset {
            worker.set_permission_preset(&preset).await?;
        }
        Ok(())
    }
    pub async fn open_session(
        &self,
        profile: RpcSessionLaunchProfile,
        retain: bool,
        now: f64,
        attach_only: bool,
        routes: impl FnOnce(&str) -> WorkerRoutes,
    ) -> Result<WorkerOpen, RpcSessionRegistryError> {
        crate::session_registry::validate_profile_paths(&profile)?;
        let (handle, worker) = {
            let mut state = lock(&self.state);
            // Both checks run before the first await, so a concurrent open naming the same durable id
            // finds this one recorded; re-opening the SAME path is an attach, not a collision.
            if let Some(id) = &profile.durable_session_id {
                if !valid_session_id(id) {
                    return Err(RpcSessionRegistryError::new(
                        "invalid_session_id",
                        None,
                        None,
                    ));
                }
                let requested = profile
                    .session_path
                    .as_deref()
                    .and_then(|path| state.known_reservation_key(path));
                let identities = state
                    .entries
                    .values()
                    .map(|entry| crate::session_registry::DurableSessionIdentity {
                        state: entry.close.state,
                        durable_session_id: entry.durable_session_id.clone(),
                        reservation_key: entry.close.reservation_key.clone(),
                    })
                    .collect::<Vec<_>>();
                crate::session_registry::check_durable_session_collision(
                    Some(id),
                    requested.as_deref(),
                    &identities,
                )?;
            }
            if let Some(path) = &profile.session_path
                && let Some(key) = state.known_reservation_key(path)
                && let Some(owner) = state.ownership.reservations.owner(&key).map(str::to_owned)
            {
                return state.attach(&owner, &key, retain, now);
            }
            if attach_only {
                return Err(RpcSessionRegistryError::new(
                    crate::rpc_types::RPC_ERROR_SESSION_NOT_HELD,
                    None,
                    Some(serde_json::json!({"reason": "session_not_held", "action": "rediscover_holder"})),
                ));
            }
            if state.entries.len() >= crate::session_worker_protocol::SESSION_WORKER_LIMITS.workers
            {
                return Err(RpcSessionRegistryError::new(
                    "too_many_sessions",
                    None,
                    None,
                ));
            }
            state.serial += 1;
            let handle = format!("rpc-{}", state.serial);
            let routes = routes(&handle);
            let worker = Arc::new(
                SessionWorkerClient::spawn(&self.spawn, self.callbacks(&handle, routes)).map_err(
                    |error| {
                        RpcSessionRegistryError::new("open_failed", Some(&error.to_string()), None)
                    },
                )?,
            );
            let stored = crate::session_registry::frozen_profile(&profile);
            state.entries.insert(
                handle.clone(),
                WorkerSessionEntry {
                    close: SessionCloseState {
                        state: RpcSessionState::Opening,
                        attachments: 1,
                        retain_on_disconnect: retain,
                        reservation_key: None,
                        writing: true,
                    },
                    identity: crate::session_registry::session_identity(&stored),
                    cwd: profile.runtime.cwd.clone(),
                    durable_session_id: profile.durable_session_id.clone(),
                    session_path: None,
                    profile: stored,
                    worker: Arc::clone(&worker),
                    last_command_at: now,
                    requested_path_key: None,
                    context_overlay: None,
                    failure: None,
                },
            );
            state
                .ownership
                .states
                .insert(handle.clone(), RpcSessionState::Opening);
            if let Some(hook) = &state.on_size_change {
                hook(state.entries.len());
            }
            (handle, worker)
        };
        match self
            .commit_open(&handle, &worker, &profile, retain, now)
            .await
        {
            Ok(open) => Ok(open),
            Err(error) => {
                set_state(
                    &mut lock(&self.state),
                    &handle,
                    RpcSessionState::Quarantined,
                );
                worker.quarantine();
                // A worker that dies before commit (for example one that cannot load its modules) closes what the
                // open is still waiting on: name why the worker died instead, as the commit check above does
                // (#4037, #1953).
                Err(self.named_worker_failure(&handle, error))
            }
        }
    }
    /// A failure that is only the worker's death is re-thrown with the worker's own recorded reason (senpi
    /// `closedByWorkerDeath`); any other failure is answered as it stands.
    fn named_worker_failure(
        &self,
        handle: &str,
        error: RpcSessionRegistryError,
    ) -> RpcSessionRegistryError {
        let failure = lock(&self.state)
            .entries
            .get(handle)
            .and_then(|entry| entry.failure.clone());
        match failure {
            Some(failure) if crate::session_worker_protocol::closed_by_worker_death(&error) => {
                RpcSessionRegistryError::new(
                    "open_failed",
                    Some(failure.trim_start_matches("open_failed: ")),
                    None,
                )
            }
            _ => error,
        }
    }
    async fn commit_open(
        &self,
        handle: &str,
        worker: &SessionWorkerClient,
        profile: &RpcSessionLaunchProfile,
        retain: bool,
        now: f64,
    ) -> Result<WorkerOpen, RpcSessionRegistryError> {
        let path = worker
            .prepare(&self.configuration, profile)
            .await
            .map_err(|error| self.open_error(handle, &error))?;
        {
            let mut state = lock(&self.state);
            if let Some(owner) = state.ownership.reservations.owner(&path).map(str::to_owned) {
                let attached = state.attach(&owner, &path, retain, now);
                set_state(&mut state, handle, RpcSessionState::Quarantined);
                drop(state);
                worker.quarantine();
                return attached;
            }
            let grant = state.reserve(handle, &path);
            if let Some(code) = grant.denial_code() {
                return Err(RpcSessionRegistryError::new(code, None, None));
            }
            if let Some(entry) = state.entries.get_mut(handle) {
                entry.close.reservation_key = Some(path.clone());
                entry.requested_path_key = profile.session_path.as_ref().map(|_| path.clone());
                entry.session_path = Some(path.clone());
            }
        }
        let snapshot = worker
            .commit()
            .await
            .map_err(|error| self.open_error(handle, &error))?;
        let mut state = lock(&self.state);
        let entry = state
            .entries
            .get_mut(handle)
            .filter(|entry| entry.close.state == RpcSessionState::Opening);
        let Some(entry) = entry else {
            let failure = state
                .entries
                .get(handle)
                .and_then(|entry| entry.failure.clone());
            return Err(failure.map_or_else(
                || RpcSessionRegistryError::new("session_closing", None, None),
                |failure| RpcSessionRegistryError::new("open_failed", Some(&failure), None),
            ));
        };
        entry.durable_session_id = snapshot.state["sessionId"].as_str().map(str::to_owned);
        if let Some(cwd) = snapshot.state["cwd"].as_str() {
            entry.cwd = cwd.to_owned();
        }
        set_state(&mut state, handle, RpcSessionState::Open);
        Ok(WorkerOpen::Created(handle.to_owned()))
    }
    /// The open only learns that its entry left \`opening\`, never why: a worker that died reports its
    /// own reason instead of \`session_closing\`.
    fn open_error(&self, handle: &str, error: &str) -> RpcSessionRegistryError {
        let failure = lock(&self.state)
            .entries
            .get(handle)
            .and_then(|entry| entry.failure.clone());
        RpcSessionRegistryError::new(
            "open_failed",
            Some(failure.as_deref().unwrap_or(error)),
            None,
        )
    }
    fn callbacks(&self, handle: &str, routes: WorkerRoutes) -> SessionWorkerCallbacks {
        let weak: Weak<Mutex<RegistryState>> = Arc::downgrade(&self.state);
        let routes = Arc::new(routes);
        let (reserve, reconcile, failure, exit) = (weak.clone(), weak.clone(), weak.clone(), weak);
        let (reserve_handle, reconcile_handle, failure_handle, exit_handle) = (
            handle.to_owned(),
            handle.to_owned(),
            handle.to_owned(),
            handle.to_owned(),
        );
        let (output_routes, close_routes, failure_routes, exit_routes) = (
            Arc::clone(&routes),
            Arc::clone(&routes),
            Arc::clone(&routes),
            routes,
        );
        let security = output_routes.security.clone();
        let presenters = output_routes.presenters.clone();
        SessionWorkerCallbacks {
            reserve: Box::new(move |path| {
                reserve
                    .upgrade()
                    .map_or(SessionWriteGrant::Conflict, |state| {
                        lock(&state).reserve(&reserve_handle, path)
                    })
            }),
            // Only a fully open entry reports live writers; anything else keeps every grant until exit.
            reconcile: Box::new(move |live, session_path| {
                let Some(state) = reconcile.upgrade() else {
                    return;
                };
                let mut state = lock(&state);
                state.ownership.reconcile(
                    &reconcile_handle,
                    &LiveWorkerPaths {
                        live_paths: live,
                        session_path,
                    },
                );
                if state.ownership.states.get(&reconcile_handle) == Some(&RpcSessionState::Open)
                    && let Some(path) = session_path
                    && let Some(entry) = state.entries.get_mut(&reconcile_handle)
                {
                    entry.close.reservation_key = Some(path.to_owned());
                    entry.session_path = Some(path.to_owned());
                }
            }),
            output: Box::new(move |record, connection| (output_routes.output)(record, connection)),
            request_close: Box::new(move || (close_routes.request_close)()),
            failure: Box::new(move |error| {
                if let Some(state) = failure.upgrade() {
                    let mut state = lock(&state);
                    if let Some(entry) = state.entries.get_mut(&failure_handle) {
                        entry.failure = Some(error.to_owned());
                    }
                    set_state(&mut state, &failure_handle, RpcSessionState::Quarantined);
                }
                eprintln!("senpi rpc session {failure_handle} quarantined: {error}");
                (failure_routes.failure)(error);
            }),
            exit: Box::new(move || {
                let failure = exit.upgrade().and_then(|state| {
                    let mut state = lock(&state);
                    let entry = state.entries.remove(&exit_handle);
                    state.ownership.exited(&exit_handle);
                    if entry.is_some()
                        && let Some(hook) = &state.on_size_change
                    {
                        hook(state.entries.len());
                    }
                    entry.and_then(|entry| entry.failure)
                });
                if let Some(error) = failure {
                    (exit_routes.terminal_failure)(&error);
                }
            }),
            security,
            presenters,
        }
    }
    pub fn mark_close(
        &self,
        handle: &str,
        detach: bool,
    ) -> Result<crate::session_teardown::CloseClaim, RpcSessionRegistryError> {
        let mut state = lock(&self.state);
        let entry = state
            .entries
            .get_mut(handle)
            .ok_or_else(|| RpcSessionRegistryError::new("unknown_session", None, None))?;
        entry.close.writing =
            crate::session_teardown::session_is_writing(Some(entry.worker.busy()), None);
        let claim = crate::session_teardown::begin_session_close(&mut entry.close, detach)?;
        let next = entry.close.state;
        state.ownership.states.insert(handle.to_owned(), next);
        Ok(claim)
    }
    /// Ends a session regardless of attachments (eviction, handoff, the worker's own close request);
    /// answers the retained path a parked session reopens by, or \`None\` when nothing was open.
    pub fn force_close(&self, handle: &str) -> Option<Option<String>> {
        let mut state = lock(&self.state);
        let entry = state
            .entries
            .get_mut(handle)
            .filter(|entry| entry.close.state == RpcSessionState::Open)?;
        let parked = if entry.close.retain_on_disconnect {
            entry.session_path.clone()
        } else {
            None
        };
        entry.close.attachments = 0;
        set_state(&mut state, handle, RpcSessionState::Closing);
        Some(parked)
    }
    /// Asks the worker to leave and waits for its actual exit; the grace deadline quarantines it.
    pub async fn close_marked(&self, handle: &str) -> Result<(), RpcSessionRegistryError> {
        let worker = {
            let state = lock(&self.state);
            state
                .entries
                .get(handle)
                .filter(|entry| entry.close.state == RpcSessionState::Closing)
                .map(|entry| Arc::clone(&entry.worker))
        };
        let worker =
            worker.ok_or_else(|| RpcSessionRegistryError::new("unknown_session", None, None))?;
        worker.close(self.close_grace_ms).await;
        Ok(())
    }
    pub fn list_sessions(&self, include_workers: bool) -> Vec<serde_json::Value> {
        lock(&self.state).entries.iter().filter(|(_,entry)|include_workers||entry.identity.kind!=maho_ext_api::SessionKind::Worker).map(|(handle,entry)|{
            let snapshot=entry.worker.snapshot();let state=snapshot.as_ref().map(|snapshot|&snapshot.state);
            let status=match entry.close.state{RpcSessionState::Opening=>"opening",RpcSessionState::Open=>"open",RpcSessionState::Closing|RpcSessionState::Quarantined=>"closing",RpcSessionState::Closed=>"closed"};
            let mut row=serde_json::json!({"sessionId":handle,"cwd":state.and_then(|state|state["cwd"].as_str()).unwrap_or(entry.cwd.as_str()),"status":status,"attachments":entry.close.attachments.max(0),"kind":if entry.identity.kind==maho_ext_api::SessionKind::Worker{"worker"}else{"interactive"},"tool_child":{"state":"none"}});
            // senpi: a worker isolate's session has no tool child of its own yet (senpi#3566 todo 4 serves the in-process host), so the row stays `none`.
            if let Some(id)=state.and_then(|state|state["sessionId"].as_str()).map(str::to_owned).or_else(||entry.durable_session_id.clone()){row["durableSessionId"]=id.into();}
            if let Some(path)=state.and_then(|state|state["sessionFile"].as_str()).map(str::to_owned).or_else(||entry.session_path.clone()){row["sessionPath"]=path.into();}
            if let Some(name)=state.and_then(|state|state["sessionName"].as_str()){row["name"]=name.into();}
            if include_workers{row["context"]=serde_json::Value::Object(crate::session_context_patch::published_session_context(&entry.identity.context,entry.context_overlay.as_ref()));}
            row
        }).collect()
    }
}

#[derive(Default)]
pub struct WorkerOwnership {
    pub states: HashMap<String, RpcSessionState>,
    pub reservations: SessionPathReservations,
}
impl WorkerOwnership {
    pub fn reserve(
        &mut self,
        handle: &str,
        path: &str,
        live: Option<&LiveWorkerPaths<'_>>,
    ) -> SessionWriteGrant {
        let Some(state) = self.states.get(handle) else {
            return SessionWriteGrant::Conflict;
        };
        if self.reservations.owner(path) == Some(handle) {
            return SessionWriteGrant::Granted;
        }
        if !matches!(state, RpcSessionState::Opening | RpcSessionState::Open) {
            return SessionWriteGrant::Conflict;
        }
        self.reservations.reserve(
            handle,
            path,
            if *state == RpcSessionState::Open {
                live
            } else {
                None
            },
        )
    }
    pub fn reconcile(&mut self, handle: &str, live: &LiveWorkerPaths<'_>) {
        if self.states.get(handle) == Some(&RpcSessionState::Open) {
            self.reservations.reconcile(handle, live);
        }
    }
    pub fn exited(&mut self, handle: &str) {
        self.states.remove(handle);
        self.reservations.release_all(handle);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn quarantine_retains_existing_writer_claim_until_actual_exit() {
        let mut registry = WorkerOwnership::default();
        registry
            .states
            .insert("one".into(), RpcSessionState::Opening);
        assert_eq!(
            registry.reserve("one", "/path", None),
            SessionWriteGrant::Granted
        );
        registry
            .states
            .insert("one".into(), RpcSessionState::Quarantined);
        registry.reconcile(
            "one",
            &LiveWorkerPaths {
                live_paths: &[],
                session_path: None,
            },
        );
        assert_eq!(
            registry.reserve("one", "/path", None),
            SessionWriteGrant::Granted
        );
        assert_eq!(
            registry.reserve("one", "/other", None),
            SessionWriteGrant::Conflict
        );
        registry.states.insert("two".into(), RpcSessionState::Open);
        assert_eq!(
            registry.reserve("two", "/path", None),
            SessionWriteGrant::Conflict
        );
        registry.exited("one");
        assert_eq!(
            registry.reserve("two", "/path", None),
            SessionWriteGrant::Granted
        );
    }
    fn registry_with_entry(state: RpcSessionState, attachments: i64) -> WorkerSessionRegistry {
        let spawn: SpawnSessionWorker = Arc::new(|| Err(std::io::Error::other("unused")));
        let registry = WorkerSessionRegistry::new(serde_json::json!({}), spawn, 0);
        let (worker, _outbox) = SessionWorkerClient::detached_for_test();
        let profile = RpcSessionLaunchProfile::default();
        lock(&registry.state).entries.insert(
            "rpc-1".into(),
            WorkerSessionEntry {
                close: SessionCloseState {
                    state,
                    attachments,
                    retain_on_disconnect: true,
                    reservation_key: None,
                    writing: false,
                },
                identity: crate::session_registry::session_identity(&profile),
                profile,
                worker: Arc::new(worker),
                durable_session_id: None,
                session_path: None,
                cwd: String::new(),
                last_command_at: 0.,
                requested_path_key: None,
                context_overlay: None,
                failure: None,
            },
        );
        registry
    }
    #[tokio::test]
    async fn attach_only_refuses_before_spawning_or_allocating_an_entry() {
        let root = tempfile::tempdir().unwrap();
        let mut profile = RpcSessionLaunchProfile::default();
        profile.runtime.cwd = root.path().to_string_lossy().into_owned();
        let spawn: SpawnSessionWorker = Arc::new(|| panic!("attach-only must not spawn"));
        let registry = WorkerSessionRegistry::new(serde_json::json!({}), spawn, 0);
        let error = registry
            .open_session(profile, false, 0., true, |_| {
                panic!("attach-only must not allocate routes")
            })
            .await
            .err()
            .unwrap();
        assert_eq!(error.code, "session_not_held");
        assert_eq!(registry.size(), 0);
        assert_eq!(lock(&registry.state).serial, 0);
    }
    // senpi `getForCommand`: polling a session nobody holds is observation and leaves its idle clock alone.
    #[test]
    fn polling_an_unattached_session_does_not_refresh_its_activity() {
        let registry = registry_with_entry(RpcSessionState::Open, 0);
        registry.get_for_command("rpc-1", "get_state", 5.).unwrap();
        assert_eq!(
            registry.with_entry("rpc-1", |entry| entry.last_command_at),
            Some(0.)
        );
        registry.get_for_command("rpc-1", "prompt", 7.).unwrap();
        assert_eq!(
            registry.with_entry("rpc-1", |entry| entry.last_command_at),
            Some(7.)
        );
        let attached = registry_with_entry(RpcSessionState::Open, 1);
        attached.get_for_command("rpc-1", "get_state", 9.).unwrap();
        assert_eq!(
            attached.with_entry("rpc-1", |entry| entry.last_command_at),
            Some(9.)
        );
    }
    // senpi `attachSession`: only an open entry takes a live attach, and joining a parked one is reported.
    #[test]
    fn attach_session_counts_only_open_entries_and_reports_a_parked_resume() {
        let registry = registry_with_entry(RpcSessionState::Open, 0);
        assert_eq!(registry.attach_session("rpc-1", 3.), Ok(true));
        assert_eq!(registry.attach_session("rpc-1", 4.), Ok(false));
        assert_eq!(
            registry.with_entry("rpc-1", |entry| entry.close.attachments),
            Some(2)
        );
        let closing = registry_with_entry(RpcSessionState::Closing, 1);
        assert_eq!(
            closing.attach_session("rpc-1", 3.).unwrap_err().code,
            "unknown_session"
        );
        assert_eq!(
            registry.attach_session("missing", 3.).unwrap_err().code,
            "unknown_session"
        );
    }
    // senpi `attach`: a worker whose binding is not installed yet cannot be joined.
    #[test]
    fn an_attach_to_a_worker_before_its_binding_is_refused() {
        let registry = registry_with_entry(RpcSessionState::Open, 1);
        assert_eq!(
            lock(&registry.state)
                .attach("rpc-1", "/p", false, 1.)
                .err()
                .map(|error| error.code),
            Some("session_path_in_use".to_owned())
        );
    }
    #[test]
    fn opening_cannot_release_claims_from_incomplete_snapshot() {
        let mut registry = WorkerOwnership::default();
        registry
            .states
            .insert("one".into(), RpcSessionState::Opening);
        registry.reserve("one", "/old", None);
        let live = LiveWorkerPaths {
            live_paths: &[],
            session_path: Some("/new"),
        };
        registry.reconcile("one", &live);
        assert_eq!(registry.reservations.count("one"), 1);
        registry.states.insert("one".into(), RpcSessionState::Open);
        registry.reconcile("one", &live);
        assert_eq!(registry.reservations.count("one"), 0);
    }
}
