use crate::session_worker_protocol::{WorkerDisplay, WorkerSnapshot};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Arc;

/// The capability a client's handshake carries when it can present an MCP grant decision: only such a connection is
/// ever asked, and only its answer counts (senpi `grant_select`).
pub const GRANT_SELECT_CAPABILITY: &str = crate::custom_capability::GRANT_SELECT_CAPABILITY;

/// `isSecurityNoticeState`: the snapshot a worker relays must be a generation stamp plus notices this host can read.
pub fn is_security_notice_state(value: &Value) -> bool {
    let Some(fields) = value.as_object() else {
        return false;
    };
    // `Number.isSafeInteger(generation) && generation >= 0`: any integral number counts, not only a JSON integer.
    let safe_generation = fields.get("generation").is_some_and(|generation| {
        generation.as_f64().is_some_and(|value| {
            value >= 0. && value.fract() == 0. && value <= 9_007_199_254_740_991.
        })
    });
    if !safe_generation {
        return false;
    }
    let Some(notices) = fields.get("notices").and_then(Value::as_array) else {
        return false;
    };
    notices.iter().all(|notice| {
        let Some(notice) = notice.as_object() else {
            return false;
        };
        if !matches!(
            notice.get("category").and_then(Value::as_str),
            Some("skill-link-blocked" | "mcp-env-withheld")
        ) {
            return false;
        }
        if notice.get("message").and_then(Value::as_str).is_none() {
            return false;
        }
        match notice.get("path") {
            // `!("path" in notice) || notice.path === undefined || typeof notice.path === "string"`: an absent path,
            // or a string. A JSON `null` is present and not a string, so it is refused.
            None | Some(Value::String(_)) => true,
            Some(_) => false,
        }
    })
}

/// Where a worker's grant-select request was routed: to one connection, or nowhere (the dialog is cancelled).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerGrantRoute {
    pub accepted: bool,
    pub connection: Option<String>,
}

/// What the bound writer lends a worker's security traffic (senpi `SessionEventWriter`'s `settleGrantDialog` /
/// `acceptWorkerSecuritySnapshot`, over its fanout's connection view). A host with no writer bridges nothing, and
/// then a worker's grant requests are forwarded as they are.
pub trait WorkerSecurityWriter: Send + Sync {
    /// Every connection attached to the session (senpi `SessionEventFanout.attachedConnections`).
    fn attached(&self, session_id: &str) -> Vec<String>;
    /// Whether `connection` registered `capability` in its own handshake (senpi `connectionHas`).
    fn has_capability(&self, connection: &str, capability: &str) -> bool;
    /// Whether the writer has no connection at all (senpi `SessionEventFanout.isEmpty`).
    fn is_empty(&self) -> bool;
    /// `settleGrantDialog`: the dialog for `id` settled, so nothing may answer it any more.
    fn settle_grant_dialog(&self, session_id: &str, id: &str);
    /// `acceptWorkerSecuritySnapshot`: the worker's notices become this session's presentation state.
    fn accept_worker_security_snapshot(&self, session_id: &str, snapshot: &Value);
}

/// One grant dialog the host routed to a connection, and the worker-side cancel that ends it.
struct PendingGrant {
    connection: Option<String>,
    cancel: Box<dyn Fn() + Send + Sync>,
}

/// The host's half of the worker grant bridge (senpi `RpcSecurityPresentations`' grant methods): the request goes to
/// the one connection that can present it, only that connection's response reaches the worker, and a connection that
/// leaves (or stops being able to present) cancels what it was asked.
pub struct WorkerGrants {
    writer: Arc<dyn WorkerSecurityWriter>,
    pending: std::sync::Mutex<BTreeMap<String, PendingGrant>>,
}

fn lock<T>(mutex: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl WorkerGrants {
    pub fn new(writer: Arc<dyn WorkerSecurityWriter>) -> Self {
        Self {
            writer,
            pending: std::sync::Mutex::new(BTreeMap::new()),
        }
    }

    /// `routeWorkerGrant`: the requesting connection when it can present the grant, else any attached connection that
    /// can, else — when this host has no connections at all — the session's own capabilities decide.
    pub fn route(
        &self,
        session_id: &str,
        request_id: &str,
        preferred: Option<&str>,
        fallback_capabilities: &[String],
        cancel: impl Fn() + Send + Sync + 'static,
    ) -> WorkerGrantRoute {
        let attached = self.writer.attached(session_id);
        let capable = |connection: &str| {
            self.writer
                .has_capability(connection, GRANT_SELECT_CAPABILITY)
        };
        let connection = match preferred {
            Some(preferred)
                if attached.iter().any(|id| id.as_str() == preferred) && capable(preferred) =>
            {
                Some(preferred.to_owned())
            }
            _ => attached.iter().find(|id| capable(id.as_str())).cloned(),
        };
        let accepted = connection.is_some()
            || (self.writer.is_empty()
                && fallback_capabilities
                    .iter()
                    .any(|capability| capability == GRANT_SELECT_CAPABILITY));
        if !accepted {
            cancel();
            return WorkerGrantRoute {
                accepted: false,
                connection: None,
            };
        }
        lock(&self.pending).insert(
            request_id.to_owned(),
            PendingGrant {
                connection: connection.clone(),
                cancel: Box::new(cancel),
            },
        );
        WorkerGrantRoute {
            accepted: true,
            connection,
        }
    }

    /// `acceptWorkerResponse`: `None` when no grant is pending under `id` (the response is an ordinary one), `false`
    /// when it came from another connection or one that can no longer present the grant, `true` when it is the answer.
    pub fn accept_response(&self, request_id: &str, connection: Option<&str>) -> Option<bool> {
        let mut pending = lock(&self.pending);
        let grant = pending.get(request_id)?;
        let asked = grant.connection.as_deref() == connection;
        let capable = connection.is_none_or(|connection| {
            self.writer
                .has_capability(connection, GRANT_SELECT_CAPABILITY)
        });
        if !asked || !capable {
            return Some(false);
        }
        pending.remove(request_id);
        Some(true)
    }

    /// `settleGrant`: the worker reported the dialog settled; nothing may answer it any more.
    pub fn settle(&self, request_id: &str) {
        lock(&self.pending).remove(request_id);
    }

    /// `cancelFor`: a connection that left the session, or stopped advertising `grant_select`, ends what it was asked.
    pub fn cancel_for(&self, connection: &str) {
        let cancelled: Vec<PendingGrant> = {
            let mut pending = lock(&self.pending);
            let ids: Vec<String> = pending
                .iter()
                .filter(|(_, grant)| grant.connection.as_deref() == Some(connection))
                .map(|(id, _)| id.clone())
                .collect();
            ids.into_iter().filter_map(|id| pending.remove(&id)).collect()
        };
        for grant in cancelled {
            (grant.cancel)();
        }
    }

    /// `#stop`: every pending dialog is cancelled, so a closing worker is never left waiting on one.
    pub fn cancel_all(&self) {
        let cancelled: Vec<PendingGrant> =
            std::mem::take(&mut *lock(&self.pending)).into_values().collect();
        for grant in cancelled {
            (grant.cancel)();
        }
    }

    /// The writer these grants belong to.
    pub fn writer(&self) -> &Arc<dyn WorkerSecurityWriter> {
        &self.writer
    }
}

/// What the worker's security bridge did with one output record (senpi `receive`'s `output` case).
enum SecurityRelay {
    /// The bridge consumed the record: the host grants its credit.
    Consumed,
    /// The record is not a snapshot this host can read: the host denies its credit and fails the worker.
    Invalid,
    /// The record goes to a client, possibly rerouted to the connection the grant was routed to.
    Forward(Option<String>),
}
#[derive(Debug, Clone, PartialEq)]
pub enum WorkerControl {
    Display(WorkerDisplay),
    CancelUi,
}
#[derive(Default)]
pub struct WorkerControls {
    display_pending: bool,
    cancel_pending: bool,
    latest_display: Option<WorkerDisplay>,
    stopped: bool,
}
impl WorkerControls {
    pub fn post(&mut self, control: WorkerControl) -> Option<WorkerControl> {
        if self.stopped {
            return None;
        }
        match &control {
            WorkerControl::Display(display) => {
                if self.display_pending {
                    self.latest_display = Some(display.clone());
                    return None;
                }
                self.display_pending = true;
            }
            WorkerControl::CancelUi => {
                if self.cancel_pending {
                    return None;
                }
                self.cancel_pending = true;
            }
        }
        Some(control)
    }
    pub fn control_done(&mut self, display: bool) -> Option<WorkerControl> {
        if display {
            self.display_pending = false;
            if let Some(latest) = self.latest_display.take() {
                return self.post(WorkerControl::Display(latest));
            }
        } else {
            self.cancel_pending = false;
        }
        None
    }
    pub fn stop(&mut self) {
        self.stopped = true;
    }
    pub fn post_envelope(
        &mut self,
        control: WorkerControl,
    ) -> Option<crate::session_worker_protocol::HostToSessionWorker> {
        self.post(control).map(|control| match control {
            WorkerControl::Display(display) => {
                crate::session_worker_protocol::HostToSessionWorker::Display { display }
            }
            WorkerControl::CancelUi => {
                crate::session_worker_protocol::HostToSessionWorker::CancelUi
            }
        })
    }
    pub fn receive_control_done(
        &mut self,
        control: crate::session_worker_protocol::WorkerControlKind,
    ) -> Option<crate::session_worker_protocol::HostToSessionWorker> {
        self.control_done(control == crate::session_worker_protocol::WorkerControlKind::Display)
            .map(|control| match control {
                WorkerControl::Display(display) => {
                    crate::session_worker_protocol::HostToSessionWorker::Display { display }
                }
                WorkerControl::CancelUi => {
                    crate::session_worker_protocol::HostToSessionWorker::CancelUi
                }
            })
    }
}
pub fn worker_busy(active_requests: usize, snapshot: Option<&WorkerSnapshot>) -> bool {
    active_requests > 0 || snapshot.is_some_and(|snapshot| snapshot.busy)
}
pub fn worker_handoff_busy(active_requests: usize, snapshot: Option<&WorkerSnapshot>) -> bool {
    active_requests > 0
        || snapshot.is_some_and(|snapshot| snapshot.handoff_busy.unwrap_or(snapshot.busy))
}
pub fn receive_reservation(
    message: crate::session_worker_protocol::SessionWorkerToHost,
    stopped: bool,
    reserve: impl FnOnce(&str) -> crate::session_worker_protocol::SessionWriteGrant,
) -> Option<crate::session_worker_protocol::SessionWorkerToHost> {
    if stopped {
        match &message {
            crate::session_worker_protocol::SessionWorkerToHost::Reserve { signal, .. }
            | crate::session_worker_protocol::SessionWorkerToHost::Snapshot { signal, .. }
            | crate::session_worker_protocol::SessionWorkerToHost::Output { signal, .. }
            | crate::session_worker_protocol::SessionWorkerToHost::Width { signal, .. }
            | crate::session_worker_protocol::SessionWorkerToHost::Capabilities {
                signal, ..
            } => signal.acknowledge(false),
            _ => {}
        }
        return None;
    }
    match message {
        crate::session_worker_protocol::SessionWorkerToHost::Reserve { path, signal } => {
            signal.acknowledge_grant(reserve(&path));
            None
        }
        message => Some(message),
    }
}
pub fn commit_output_activity(
    snapshot: &mut Option<WorkerSnapshot>,
    replacement: Option<WorkerSnapshot>,
    busy: bool,
    handoff_busy: Option<bool>,
    streaming: bool,
) {
    if let Some(replacement) = replacement {
        *snapshot = Some(replacement);
    } else if let Some(snapshot) = snapshot {
        snapshot.busy = busy;
        snapshot.handoff_busy = handoff_busy;
        snapshot.streaming = streaming;
        snapshot.state["isStreaming"] = streaming.into();
    }
}
pub fn receive_snapshot(
    snapshot: &mut Option<WorkerSnapshot>,
    replacement: WorkerSnapshot,
    signal: &crate::session_worker_signals::WorkerSignal,
    settled: bool,
    reconcile: impl FnOnce(&[String]),
    notify_settled: impl FnOnce(),
) {
    *snapshot = Some(replacement);
    reconcile(
        &snapshot
            .as_ref()
            .expect("committed snapshot")
            .live_session_paths,
    );
    signal.acknowledge(true);
    if settled {
        notify_settled();
    }
}
pub async fn settle_output_credit(
    signal: &crate::session_worker_signals::WorkerSignal,
    consumed: impl std::future::Future<Output = Result<(), String>>,
    fail: impl FnOnce(&str),
) {
    match consumed.await {
        Ok(()) => signal.acknowledge(true),
        Err(error) => {
            signal.acknowledge(false);
            fail(&error);
        }
    }
}
/// Spawns one isolated session worker process with piped stdin/stdout (senpi constructs a
/// \`node:worker_threads\` Worker; the native worker is a child process of the same binary).
pub type SpawnSessionWorker =
    std::sync::Arc<dyn Fn() -> std::io::Result<tokio::process::Child> + Send + Sync>;
/// Lifecycle hooks the owning registry installs on every worker it allocates (senpi \`SessionWorkerCallbacks\`),
/// plus the output/close routes senpi reaches through the bound \`SessionEventWriter\`.
/// Write-reservation hook: grants or refuses one session path.
pub type ReserveCallback =
    Box<dyn Fn(&str) -> crate::session_worker_protocol::SessionWriteGrant + Send + Sync>;
/// Snapshot hook: republishes the paths this worker still writes.
pub type ReconcileCallback = Box<dyn Fn(&[String], Option<&str>) + Send + Sync>;
pub struct SessionWorkerCallbacks {
    pub reserve: ReserveCallback,
    /// Every snapshot republishes which paths this worker still writes.
    pub reconcile: ReconcileCallback,
    pub output: Box<dyn Fn(serde_json::Value, Option<String>) + Send + Sync>,
    pub request_close: Box<dyn Fn() + Send + Sync>,
    pub failure: Box<dyn Fn(&str) + Send + Sync>,
    pub exit: Box<dyn Fn() + Send + Sync>,
    /// The bound writer's security bridge (senpi `SessionEventWriter`'s grant methods); `None` forwards a worker's
    /// grant traffic to the clients unchanged.
    pub security: Option<Arc<dyn WorkerSecurityWriter>>,
}
struct ClientState {
    snapshot: Option<WorkerSnapshot>,
    stopped: bool,
    closing: bool,
    exited: bool,
    binding_ready: bool,
    display_revision: f64,
    capabilities: Vec<String>,
    /// The session this worker was bound to; absent before `bind`.
    bound_session: Option<String>,
    controls: WorkerControls,
    connection_roles: Option<crate::session_binding::SharedConnectionRoles>,
}
struct ClientInner {
    state: std::sync::Mutex<ClientState>,
    requests: std::sync::Mutex<crate::session_worker_requests::SessionWorkerRequests>,
    outbox: tokio::sync::mpsc::UnboundedSender<String>,
    kill: std::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    exited: tokio::sync::watch::Receiver<bool>,
    settled: tokio::sync::watch::Sender<u64>,
    callbacks: SessionWorkerCallbacks,
    /// The grant dialogs this worker is waiting on, when a writer bridges them.
    grants: Option<WorkerGrants>,
    started: std::time::Instant,
}
/// Host-side handle of one session worker process (senpi \`SessionWorkerClient\`).
pub struct SessionWorkerClient {
    inner: std::sync::Arc<ClientInner>,
}
fn wire_signal(message: &crate::session_worker_protocol::SessionWorkerWire) -> Option<u64> {
    use crate::session_worker_protocol::SessionWorkerWire as W;
    match message {
        W::Reserve { signal, .. }
        | W::Snapshot { signal, .. }
        | W::Output { signal, .. }
        | W::Width { signal, .. }
        | W::Capabilities { signal, .. } => Some(*signal),
        _ => None,
    }
}
impl ClientInner {
    fn now(&self) -> u64 {
        u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX)
    }
    fn send(&self, message: &crate::session_worker_protocol::HostToSessionWorker) {
        if let Ok(line) = serde_json::to_string(message) {
            let _ = self.outbox.send(format!("{line}\n"));
        }
    }
    fn credit(&self, signal: u64, code: i32, display: Option<WorkerDisplay>) {
        self.send(
            &crate::session_worker_protocol::HostToSessionWorker::Credit {
                signal,
                code,
                display,
            },
        );
    }
    fn display(&self) -> WorkerDisplay {
        let mut state = lock(&self.state);
        state.display_revision += 1.;
        WorkerDisplay {
            revision: state.display_revision,
            width: 80.,
            rendered: false,
            capabilities: state.capabilities.clone(),
        }
    }
    fn post(&self, control: WorkerControl) {
        let envelope = lock(&self.state).controls.post_envelope(control);
        if let Some(envelope) = envelope {
            self.send(&envelope);
        }
    }
    fn quarantine(&self) {
        lock(&self.state).stopped = true;
        lock(&self.requests).close_with_error("session_closing");
        // A quarantined worker never answers a grant dialog, so every dialog it was asked is cancelled here.
        if let Some(grants) = &self.grants {
            grants.cancel_all();
        }
        if let Some(kill) = lock(&self.kill).take() {
            let _ = kill.send(());
        }
    }
    fn fail(&self, error: &str) {
        {
            let state = lock(&self.state);
            if state.stopped {
                return;
            }
        }
        (self.callbacks.failure)(error);
        self.quarantine();
    }
    fn exited_now(&self) {
        let unexpected = {
            let mut state = lock(&self.state);
            let unexpected = !state.stopped && !state.closing;
            state.exited = true;
            unexpected
        };
        if unexpected {
            self.fail("session_worker_exited");
        }
        {
            let mut state = lock(&self.state);
            state.stopped = true;
            state.controls.stop();
        }
        lock(&self.requests).close_with_error("session_worker_exited");
        self.settled.send_modify(|count| *count += 1);
        (self.callbacks.exit)();
    }
    /// senpi `receive`'s `output` case before publication: the worker's security traffic is the host's to route, and a
    /// grant-select request goes to the one connection that can present it.
    fn relay_security(&self, record: &Value, connection: Option<&str>) -> SecurityRelay {
        let (Some(grants), Some(session_id)) = (&self.grants, lock(&self.state).bound_session.clone())
        else {
            return SecurityRelay::Forward(connection.map(str::to_owned));
        };
        let kind = record.get("type").and_then(Value::as_str);
        if kind == Some("security_grant_settled") {
            let Some(id) = record.get("id").and_then(Value::as_str) else {
                return SecurityRelay::Forward(connection.map(str::to_owned));
            };
            grants.writer().settle_grant_dialog(&session_id, id);
            grants.settle(id);
            return SecurityRelay::Consumed;
        }
        if kind == Some("security_notice_snapshot") {
            let Some(snapshot) = record.get("snapshot") else {
                return SecurityRelay::Invalid;
            };
            if !is_security_notice_state(snapshot) {
                return SecurityRelay::Invalid;
            }
            grants
                .writer()
                .accept_worker_security_snapshot(&session_id, snapshot);
            return SecurityRelay::Consumed;
        }
        if kind == Some("extension_ui_request")
            && record.get("method").and_then(Value::as_str) == Some("select")
            && record.get("grantSelect").and_then(Value::as_bool) == Some(true)
        {
            let Some(id) = record.get("id").and_then(Value::as_str) else {
                return SecurityRelay::Forward(connection.map(str::to_owned));
            };
            let fallback = lock(&self.state).capabilities.clone();
            let cancel_id = id.to_owned();
            let client = self.outbox.clone();
            let target = grants.route(
                &session_id,
                id,
                connection,
                &fallback,
                move || {
                    // `post({ type: "cancel_grant", id })`: the worker ends the dialog nobody can present.
                    if let Ok(line) = serde_json::to_string(
                        &crate::session_worker_protocol::HostToSessionWorker::CancelGrant {
                            id: cancel_id.clone(),
                        },
                    ) {
                        let _ = client.send(format!("{line}\n"));
                    }
                },
            );
            if !target.accepted {
                return SecurityRelay::Consumed;
            }
            return SecurityRelay::Forward(target.connection);
        }
        SecurityRelay::Forward(connection.map(str::to_owned))
    }
    /// senpi `acceptWorkerGrantResponse`: whether this response may reach the worker.
    fn accept_grant_response(&self, id: &str, connection: Option<&str>) -> Option<bool> {
        self.grants
            .as_ref()
            .and_then(|grants| grants.accept_response(id, connection))
    }
    fn receive(&self, message: crate::session_worker_protocol::SessionWorkerWire) {
        use crate::session_worker_protocol::SessionWorkerWire as W;
        if lock(&self.state).stopped {
            if let Some(signal) = wire_signal(&message) {
                self.credit(
                    signal,
                    crate::session_worker_protocol::WORKER_CREDIT_CODES.conflict,
                    None,
                );
            }
            return;
        }
        match message {
            W::Prepared { .. } | W::Ready { .. } | W::Result { .. } => {
                if let Ok(value) = serde_json::to_value(&message) {
                    lock(&self.requests).receive(&value);
                }
            }
            W::Reserve { path, signal } => {
                let grant = (self.callbacks.reserve)(&path);
                self.credit(
                    signal,
                    crate::session_worker_protocol::worker_credit_code(grant),
                    None,
                );
            }
            W::Snapshot {
                snapshot,
                signal,
                settled,
            } => {
                (self.callbacks.reconcile)(
                    snapshot.live_session_paths.as_slice(),
                    snapshot.session_path.as_deref(),
                );
                lock(&self.state).snapshot = Some(snapshot);
                self.credit(
                    signal,
                    crate::session_worker_protocol::WORKER_CREDIT_CODES.granted,
                    None,
                );
                if settled == Some(true) {
                    self.settled.send_modify(|count| *count += 1);
                }
            }
            W::ControlDone { control } => {
                let next = lock(&self.state).controls.receive_control_done(control);
                if let Some(next) = next {
                    self.send(&next);
                }
            }
            W::Output {
                record,
                connection,
                signal,
                activity,
                snapshot,
            } => {
                if !lock(&self.state).binding_ready {
                    self.credit(
                        signal,
                        crate::session_worker_protocol::WORKER_CREDIT_CODES.conflict,
                        None,
                    );
                    return;
                }
                // Identity and activity commit before publication; clients may attach or disconnect on that event.
                if let Some(replacement) = &snapshot {
                    (self.callbacks.reconcile)(
                        replacement.live_session_paths.as_slice(),
                        replacement.session_path.as_deref(),
                    );
                }
                {
                    let mut state = lock(&self.state);
                    commit_output_activity(
                        &mut state.snapshot,
                        snapshot,
                        activity.busy,
                        activity.handoff_busy,
                        activity.streaming,
                    );
                }
                // The worker's security traffic is the host's to route: a grant dialog reaches only the connection
                // that can present it, and a settled grant or a notice snapshot never reaches a client at all.
                let target = match self.relay_security(&record, connection.as_deref()) {
                    SecurityRelay::Forward(target) => target,
                    SecurityRelay::Consumed => {
                        self.credit(
                            signal,
                            crate::session_worker_protocol::WORKER_CREDIT_CODES.granted,
                            None,
                        );
                        return;
                    }
                    SecurityRelay::Invalid => {
                        self.credit(
                            signal,
                            crate::session_worker_protocol::WORKER_CREDIT_CODES.conflict,
                            None,
                        );
                        self.fail("invalid_security_notice_snapshot");
                        return;
                    }
                };
                (self.callbacks.output)(record, target);
                self.credit(
                    signal,
                    crate::session_worker_protocol::WORKER_CREDIT_CODES.granted,
                    None,
                );
            }
            W::Capabilities {
                connection,
                capabilities,
                signal,
            } => {
                // A handshake with no connection is the worker's own list, which is what an unregistered host routes
                // a grant dialog by (senpi `options.capabilities`).
                if connection.is_none() {
                    lock(&self.state).capabilities = capabilities.clone();
                }
                // The isolate already refused a changed role; a refusal here means the two registries drifted.
                let roles = lock(&self.state).connection_roles.clone();
                if let Some(roles) = roles {
                    let id =
                        crate::connection_roles::role_connection(connection.as_deref()).to_owned();
                    if !lock(&roles).declare(&id, &capabilities) {
                        eprintln!(
                            "senpi rpc worker role drift: connection {id} declared a changed role"
                        );
                    }
                }
                let display = self.display();
                self.credit(
                    signal,
                    crate::session_worker_protocol::WORKER_CREDIT_CODES.granted,
                    Some(display),
                );
            }
            W::Width { signal, .. } => {
                let display = self.display();
                self.credit(
                    signal,
                    crate::session_worker_protocol::WORKER_CREDIT_CODES.granted,
                    Some(display),
                );
            }
            W::RequestClose => (self.callbacks.request_close)(),
            W::Failure { error } => self.fail(&error),
        }
    }
}
impl SessionWorkerClient {
    /// Starts the worker process and its pipe pumps; only the process's actual exit releases ownership.
    pub fn spawn(
        spawn: &SpawnSessionWorker,
        callbacks: SessionWorkerCallbacks,
    ) -> std::io::Result<Self> {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
        let mut child = spawn()?;
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| std::io::Error::other("session worker stdin is not piped"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| std::io::Error::other("session worker stdout is not piped"))?;
        let (outbox, mut lines) = tokio::sync::mpsc::unbounded_channel::<String>();
        let (kill, killed) = tokio::sync::oneshot::channel::<()>();
        let (exited_tx, exited) = tokio::sync::watch::channel(false);
        let grants = callbacks.security.clone().map(WorkerGrants::new);
        let inner = std::sync::Arc::new(ClientInner {
            state: std::sync::Mutex::new(ClientState {
                snapshot: None,
                stopped: false,
                closing: false,
                exited: false,
                binding_ready: false,
                display_revision: 0.,
                capabilities: Vec::new(),
                bound_session: None,
                controls: WorkerControls::default(),
                connection_roles: None,
            }),
            requests: Default::default(),
            outbox,
            kill: std::sync::Mutex::new(Some(kill)),
            exited,
            settled: tokio::sync::watch::channel(0).0,
            callbacks,
            grants,
            started: std::time::Instant::now(),
        });
        tokio::spawn(async move {
            while let Some(line) = lines.recv().await {
                if stdin.write_all(line.as_bytes()).await.is_err() || stdin.flush().await.is_err() {
                    break;
                }
            }
        });
        let reader = std::sync::Arc::downgrade(&inner);
        let pump = tokio::spawn(async move {
            let mut lines = tokio::io::BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let Some(inner) = reader.upgrade() else { break };
                match serde_json::from_str::<crate::session_worker_protocol::SessionWorkerWire>(
                    &line,
                ) {
                    Ok(message) => inner.receive(message),
                    Err(error) => inner.fail(&format!("session_worker_protocol_error: {error}")),
                }
            }
        });
        let watcher = std::sync::Arc::downgrade(&inner);
        tokio::spawn(async move {
            tokio::select! {_=child.wait()=>{},_=killed=>{let _=child.start_kill();let _=child.wait().await;}}
            let _ = pump.await;
            if let Some(inner) = watcher.upgrade() {
                inner.exited_now();
            }
            let _ = exited_tx.send(true);
        });
        Ok(Self { inner })
    }
    async fn request(&self, message: serde_json::Value) -> Result<serde_json::Value, String> {
        let now = self.inner.now();
        let outbox = self.inner.outbox.clone();
        let weak = std::sync::Arc::downgrade(&self.inner);
        let receiver = lock(&self.inner.requests)
            .submit_with_timeout(
                &message,
                now,
                move |value| {
                    outbox
                        .send(format!("{value}\n"))
                        .map_err(|error| error.to_string())
                },
                move || {
                    if let Some(inner) = weak.upgrade() {
                        inner.fail("session_worker_request_timeout");
                    }
                },
            )
            .map_err(|error| error.to_string())?;
        receiver.await.map_err(|_| "session_closing".to_owned())?
    }
    pub async fn prepare(
        &self,
        configuration: &serde_json::Value,
        profile: &crate::session_registry::RpcSessionLaunchProfile,
    ) -> Result<String, String> {
        let result=self.request(serde_json::json!({"type":"prepare","configuration":configuration,"profile":crate::session_worker_protocol::profile_to_wire(profile)})).await?;
        match (result["type"].as_str(), result["sessionPath"].as_str()) {
            (Some("prepared"), Some(path)) => Ok(path.to_owned()),
            _ => Err("Invalid worker prepare response".into()),
        }
    }
    pub async fn commit(&self) -> Result<WorkerSnapshot, String> {
        let result = self.request(serde_json::json!({"type":"commit"})).await?;
        if result["type"] != "ready" {
            return Err("Invalid worker commit response".into());
        }
        let snapshot: WorkerSnapshot = serde_json::from_value(result["snapshot"].clone())
            .map_err(|error| error.to_string())?;
        lock(&self.inner.state).snapshot = Some(snapshot.clone());
        Ok(snapshot)
    }
    /// The host's connection-roles registry, checked against the worker's `capabilities` declarations (senpi `options.connectionRoles`).
    pub fn set_connection_roles(&self, roles: crate::session_binding::SharedConnectionRoles) {
        lock(&self.inner.state).connection_roles = Some(roles);
    }
    /// Installs the worker's routing binding; output is accepted from this point on.
    pub async fn bind(
        &self,
        session_id: &str,
        capabilities: Vec<String>,
        connection: Option<String>,
        roles: Option<Vec<String>>,
    ) -> Result<(), String> {
        {
            let mut state = lock(&self.inner.state);
            state.capabilities = capabilities;
            state.bound_session = Some(session_id.to_owned());
            state.binding_ready = true;
        }
        let mut message = serde_json::json!({"type":"bind","sessionId":session_id,"display":self.inner.display()});
        if let Some(connection) = connection {
            message["connection"] = connection.into();
        }
        if let Some(roles) = roles {
            message["roles"] = serde_json::json!(roles);
        }
        self.request(message).await.map(|_| ())
    }
    pub async fn command(
        &self,
        command: serde_json::Value,
        connection: Option<String>,
        roles: Option<Vec<String>>,
    ) -> Result<(), String> {
        // A response to a grant dialog belongs to the worker only when it came from the connection the dialog was
        // routed to (senpi `acceptWorkerGrantResponse`); any other answer is dropped here.
        if command.get("type").and_then(Value::as_str) == Some("extension_ui_response")
            && let Some(id) = command.get("id").and_then(Value::as_str)
            && self.inner.accept_grant_response(id, connection.as_deref()) == Some(false)
        {
            return Ok(());
        }
        let mut message =
            serde_json::json!({"type":"command","command":command,"display":self.inner.display()});
        if let Some(connection) = connection {
            message["connection"] = connection.into();
        }
        if let Some(roles) = roles {
            message["roles"] = serde_json::json!(roles);
        }
        self.request(message).await.map(|_| ())
    }
    pub fn cancel_pending_extension_ui_requests(&self) {
        self.inner.post(WorkerControl::CancelUi);
    }
    pub fn rerender_components(&self) {
        let display = self.inner.display();
        self.inner.post(WorkerControl::Display(display));
    }
    pub fn dispose(&self) {
        self.inner.post(WorkerControl::CancelUi);
    }
    pub fn snapshot(&self) -> Option<WorkerSnapshot> {
        lock(&self.inner.state).snapshot.clone()
    }
    pub fn busy(&self) -> bool {
        worker_busy(
            lock(&self.inner.requests).active_count(),
            lock(&self.inner.state).snapshot.as_ref(),
        )
    }
    pub fn handoff_busy(&self) -> bool {
        worker_handoff_busy(
            lock(&self.inner.requests).active_count(),
            lock(&self.inner.state).snapshot.as_ref(),
        )
    }
    pub fn is_stopped(&self) -> bool {
        lock(&self.inner.state).stopped
    }
    /// Bumped whenever the worker reports a settled turn or exits.
    pub fn settled(&self) -> tokio::sync::watch::Receiver<u64> {
        self.inner.settled.subscribe()
    }
    /// Deadline requests termination, but only the actual exit releases ownership.
    pub async fn close(&self, grace_ms: u64) {
        let mut exited = self.inner.exited.clone();
        let already = {
            let mut state = lock(&self.inner.state);
            let already = state.exited || state.closing || state.stopped;
            state.closing = true;
            already
        };
        if !already {
            self.inner
                .send(&crate::session_worker_protocol::HostToSessionWorker::Close);
        }
        if tokio::time::timeout(
            std::time::Duration::from_millis(grace_ms),
            exited.wait_for(|exited| *exited),
        )
        .await
        .is_err()
        {
            self.inner.quarantine();
            let _ = exited.wait_for(|exited| *exited).await;
        }
    }
    pub fn quarantine(&self) {
        self.inner.quarantine();
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn display(revision: f64) -> WorkerControl {
        WorkerControl::Display(WorkerDisplay {
            revision,
            width: 80.,
            rendered: false,
            capabilities: vec![],
        })
    }
    #[test]
    fn output_activity_updates_identity_before_publication_without_creating_runtime() {
        let mut snapshot = None;
        commit_output_activity(&mut snapshot, None, true, Some(true), true);
        assert!(snapshot.is_none());
        let replacement = WorkerSnapshot {
            state: serde_json::json!({"sessionId":"durable","isStreaming":false}),
            session_path: Some("/session".into()),
            live_session_paths: vec!["/session".into()],
            busy: false,
            handoff_busy: Some(false),
            streaming: false,
        };
        commit_output_activity(&mut snapshot, Some(replacement), true, Some(true), true);
        assert!(!snapshot.as_ref().unwrap().busy);
        commit_output_activity(&mut snapshot, None, true, Some(true), true);
        let snapshot = snapshot.unwrap();
        assert_eq!(snapshot.state["sessionId"], "durable");
        assert_eq!(snapshot.state["isStreaming"], true);
        assert!(snapshot.busy);
        assert_eq!(snapshot.session_path.as_deref(), Some("/session"));
    }
    #[test]
    fn control_debt_coalesces_latest_display_and_cancel_independently() {
        let mut controls = WorkerControls::default();
        assert_eq!(controls.post(display(1.)), Some(display(1.)));
        assert!(controls.post(display(2.)).is_none());
        assert!(controls.post(display(3.)).is_none());
        assert_eq!(
            controls.post(WorkerControl::CancelUi),
            Some(WorkerControl::CancelUi)
        );
        assert!(controls.post(WorkerControl::CancelUi).is_none());
        assert_eq!(controls.control_done(true), Some(display(3.)));
        assert!(controls.post(display(4.)).is_none());
        controls.control_done(false);
        assert_eq!(
            controls.post(WorkerControl::CancelUi),
            Some(WorkerControl::CancelUi)
        );
        controls.stop();
        assert!(controls.control_done(true).is_none());
        assert!(controls.post(display(5.)).is_none());
    }
    #[test]
    fn handoff_uses_specific_busy_field_but_request_debt_always_holds() {
        let mut snapshot = WorkerSnapshot {
            state: serde_json::json!({}),
            session_path: None,
            live_session_paths: vec![],
            busy: true,
            handoff_busy: Some(false),
            streaming: false,
        };
        assert!(worker_busy(0, Some(&snapshot)));
        assert!(!worker_handoff_busy(0, Some(&snapshot)));
        assert!(worker_handoff_busy(1, Some(&snapshot)));
        snapshot.handoff_busy = None;
        assert!(worker_handoff_busy(0, Some(&snapshot)));
        assert!(!worker_busy(0, None));
    }

    /// The writer a worker's grant dialogs are routed by, with the two connections a shared host has.
    struct FakeWriter {
        attached: Vec<String>,
        capable: Vec<String>,
        empty: bool,
        settled: std::sync::Mutex<Vec<String>>,
        snapshots: std::sync::Mutex<Vec<Value>>,
    }

    impl WorkerSecurityWriter for FakeWriter {
        fn attached(&self, _: &str) -> Vec<String> {
            self.attached.clone()
        }
        fn has_capability(&self, connection: &str, capability: &str) -> bool {
            capability == GRANT_SELECT_CAPABILITY
                && self.capable.iter().any(|id| id == connection)
        }
        fn is_empty(&self) -> bool {
            self.empty
        }
        fn settle_grant_dialog(&self, _: &str, id: &str) {
            self.settled.lock().unwrap().push(id.to_owned());
        }
        fn accept_worker_security_snapshot(&self, _: &str, snapshot: &Value) {
            self.snapshots.lock().unwrap().push(snapshot.clone());
        }
    }

    fn grants(attached: &[&str], capable: &[&str], empty: bool) -> WorkerGrants {
        WorkerGrants::new(Arc::new(FakeWriter {
            attached: attached.iter().map(|id| (*id).to_owned()).collect(),
            capable: capable.iter().map(|id| (*id).to_owned()).collect(),
            empty,
            settled: std::sync::Mutex::new(Vec::new()),
            snapshots: std::sync::Mutex::new(Vec::new()),
        }))
    }

    #[test]
    fn a_grant_dialog_goes_to_the_connection_that_can_present_it() {
        let grants = grants(&["peer-a", "peer-b"], &["peer-b"], false);
        let cancelled = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = std::sync::Arc::clone(&cancelled);
        let route = grants.route("s", "ui-1", Some("peer-a"), &[], move || {
            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        });
        // The requesting connection cannot present the grant, so the one that can gets it instead.
        assert_eq!(
            route,
            WorkerGrantRoute {
                accepted: true,
                connection: Some("peer-b".to_owned())
            }
        );
        assert_eq!(cancelled.load(std::sync::atomic::Ordering::SeqCst), 0);
        // Only that connection's answer reaches the worker.
        assert_eq!(grants.accept_response("ui-1", Some("peer-a")), Some(false));
        assert_eq!(grants.accept_response("ui-1", Some("peer-b")), Some(true));
        assert_eq!(grants.accept_response("ui-1", Some("peer-b")), None);
    }

    #[test]
    fn a_grant_dialog_nobody_can_present_is_cancelled_in_the_worker() {
        let grants = grants(&["peer-a"], &[], false);
        let cancelled = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = std::sync::Arc::clone(&cancelled);
        let route = grants.route("s", "ui-1", Some("peer-a"), &[], move || {
            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        });
        assert_eq!(
            route,
            WorkerGrantRoute {
                accepted: false,
                connection: None
            }
        );
        assert_eq!(cancelled.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(grants.accept_response("ui-1", Some("peer-a")), None);
    }

    #[test]
    fn with_no_connections_the_sessions_own_capabilities_decide() {
        let grants = grants(&[], &[], true);
        let route = grants.route(
            "s",
            "ui-1",
            None,
            &[GRANT_SELECT_CAPABILITY.to_owned()],
            || panic!("a capable session keeps its dialog"),
        );
        assert_eq!(
            route,
            WorkerGrantRoute {
                accepted: true,
                connection: None
            }
        );
        // An untargeted dialog is answered by whoever takes it.
        assert_eq!(grants.accept_response("ui-1", None), Some(true));
    }

    #[test]
    fn a_connection_that_leaves_cancels_what_it_was_asked_and_a_settled_dialog_is_forgotten() {
        let grants = grants(&["peer-a"], &["peer-a"], false);
        let cancelled = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = std::sync::Arc::clone(&cancelled);
        grants.route("s", "ui-1", Some("peer-a"), &[], move || {
            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        });
        grants.route("s", "ui-2", Some("peer-a"), &[], || {});
        grants.settle("ui-2");
        assert_eq!(grants.accept_response("ui-2", Some("peer-a")), None);
        grants.cancel_for("peer-a");
        assert_eq!(cancelled.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(grants.accept_response("ui-1", Some("peer-a")), None);
    }

    #[test]
    fn only_a_readable_security_notice_snapshot_crosses_the_worker_wire() {
        for snapshot in [
            serde_json::json!({"generation":0,"notices":[]}),
            serde_json::json!({"generation":1.0,"notices":[]}),
            serde_json::json!({"generation":2,"notices":[{"category":"skill-link-blocked","message":"m","path":"/p"}]}),
            serde_json::json!({"generation":1,"notices":[{"category":"mcp-env-withheld","message":"m"}]}),
        ] {
            assert!(is_security_notice_state(&snapshot), "{snapshot}");
        }
        for snapshot in [
            serde_json::json!({}),
            serde_json::json!({"generation":-1,"notices":[]}),
            serde_json::json!({"generation":1.5,"notices":[]}),
            serde_json::json!({"generation":1,"notices":{}}),
            serde_json::json!({"generation":1,"notices":[{"category":"other","message":"m"}]}),
            serde_json::json!({"generation":1,"notices":[{"category":"mcp-env-withheld"}]}),
            serde_json::json!({"generation":1,"notices":[{"category":"mcp-env-withheld","message":"m","path":7}]}),
            serde_json::json!({"generation":1,"notices":[{"category":"mcp-env-withheld","message":"m","path":null}]}),
            serde_json::json!("snapshot"),
        ] {
            assert!(!is_security_notice_state(&snapshot), "{snapshot}");
        }
    }
}
