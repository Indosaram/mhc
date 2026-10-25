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
        entry.last_command_at = now;
        Ok(Arc::clone(&entry.worker))
    }
    pub async fn open_session(
        &self,
        profile: RpcSessionLaunchProfile,
        retain: bool,
        now: f64,
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
                    entry.and_then(|entry| entry.failure)
                });
                if let Some(error) = failure {
                    (exit_routes.terminal_failure)(&error);
                }
            }),
            security,
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
