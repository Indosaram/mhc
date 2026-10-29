//! Controls on delegated tasks (`docs/cleanroom/specs/task-control.md` §1-§3): `task_send`
//! (queue, steer or revive), the full `task_cancel` contract, `/task-kill`, the gateway steering
//! channel and the delegated-run inbox (the §1.1 exception to `gateway.md` §5).
use crate::manager::{TaskManager, WrittenSteer};
use crate::record::{PendingMessage, TaskRecord, TaskStatus, now_ms, pid_alive};
use crate::runner::BoxFuture;
use crate::settings::TASK_ID_ENV;
use maho_ext_api::{ExtensionContext, ExtensionMode, ExtensionUiDialogOptions, NotificationType};
use maho_gateway::{Delivery, Endpoint, Mode, Sender, Store};
use serde_json::{Value, json};
use maho_ext_api::{CustomMessage, SendMessageOptions};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// Largest `task_send` message (UTF-8 bytes).
pub const MAX_MESSAGE_BYTES: usize = 32 * 1024;
/// Most ids suggested by a `not_found` result.
pub const MAX_CANDIDATES: usize = 10;
/// Reason recorded by `/task-kill`.
pub const KILL_REASON: &str = "/task-kill";
/// How long a cancel waits for a run owned here to settle.
const CANCEL_SETTLE: Duration = Duration::from_secs(10);

fn lock<T>(mutex: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Delivers a steering message into a running child's current turn.
pub trait SteerChannel: Send + Sync {
    /// Writes `text` for the child session `child_session` from `parent_session`. `Some(path)` =
    /// written to that inbox file; `None` = no live channel (or a write error).
    fn steer(&self, child_session: &str, parent_session: &str, text: &str) -> Option<PathBuf>;
}

/// The session gateway as steering channel (spec §1.1).
#[derive(Clone, Debug)]
pub struct GatewaySteer {
    pub store: Store,
    /// `gateway.enabled` (and `MAHO_GATEWAY`).
    pub enabled: bool,
}

impl SteerChannel for GatewaySteer {
    fn steer(&self, child_session: &str, parent_session: &str, text: &str) -> Option<PathBuf> {
        if !self.enabled {
            return None;
        }
        let live = self
            .store
            .live()
            .iter()
            .any(|endpoint| endpoint.session_id == child_session);
        if !live {
            return None;
        }
        let delivery = Delivery::new(
            text,
            Sender::Agent {
                session_id: parent_session.to_owned(),
                name: None,
            },
            Mode::Steer,
            1,
        );
        self.store.deliver(child_session, &delivery).ok()
    }
}

/// The calling session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Caller {
    pub session_id: String,
    /// Root session of the caller (itself for a top-level session).
    pub root_session: String,
}

/// One control outcome: `kind` (reported in the details), the text, and the details.
#[derive(Clone, Debug, PartialEq)]
pub struct Outcome {
    pub kind: &'static str,
    pub text: String,
    pub details: Value,
}

impl Outcome {
    fn new(kind: &'static str, text: String, mut details: Value) -> Self {
        if let Some(map) = details.as_object_mut() {
            map.insert("kind".into(), json!(kind));
        }
        Self {
            kind,
            text,
            details,
        }
    }
    /// Whether the outcome is a caller error (bad arguments, unknown task, wrong session).
    pub fn is_error(&self) -> bool {
        matches!(self.kind, "invalid_arguments" | "not_found" | "scope_denied")
    }
}

/// `to` as task id, then as task name (most recent first).
pub fn resolve(manager: &TaskManager, to: &str) -> Option<TaskRecord> {
    let to = to.trim();
    if to.is_empty() {
        return None;
    }
    if let Some(record) = manager.get(to) {
        return Some(record);
    }
    manager
        .list()
        .into_iter()
        .rev()
        .find(|record| record.name.as_deref() == Some(to))
}

fn owned_by(record: &TaskRecord, session: &str) -> bool {
    record.owner_session.as_deref().is_none_or(|owner| owner == session)
}

/// Ids of the caller's active tasks (at most [`MAX_CANDIDATES`]).
pub fn candidates(manager: &TaskManager, caller: &Caller) -> Vec<String> {
    manager
        .list()
        .into_iter()
        .filter(|record| record.status.is_active() && owned_by(record, &caller.session_id))
        .map(|record| record.id)
        .take(MAX_CANDIDATES)
        .collect()
}

fn not_found(manager: &TaskManager, caller: &Caller, to: &str) -> Outcome {
    let ids = candidates(manager, caller);
    let text = if ids.is_empty() {
        format!("No task \"{to}\" found, and this session has no active tasks.")
    } else {
        format!("No task \"{to}\" found. Active tasks of this session: {}", ids.join(", "))
    };
    Outcome::new("not_found", text, json!({"to": to, "candidates": ids}))
}

/// Ownership (spec §1): the owning session, or the root session of that chain.
fn scope_check(record: &TaskRecord, caller: &Caller, all_sessions: bool) -> Option<Outcome> {
    let owner = record.owner_session.as_deref()?;
    if all_sessions
        || owner == caller.session_id
        || record.root_session.as_deref() == Some(caller.session_id.as_str())
    {
        return None;
    }
    Some(Outcome::new(
        "scope_denied",
        format!(
            "Task {} belongs to session {owner}; pass all_sessions: true to message it from this session.",
            record.id
        ),
        json!({"id": record.id, "owner_session": owner}),
    ))
}

/// Validates a `task_send` message (spec §1 "Limits").
///
/// # Errors
/// The `invalid_arguments` outcome.
pub fn check_message(message: &str) -> Result<(), Outcome> {
    if message.trim().is_empty() {
        return Err(Outcome::new(
            "invalid_arguments",
            "The message is empty.".into(),
            json!({}),
        ));
    }
    if message.len() > MAX_MESSAGE_BYTES {
        return Err(Outcome::new(
            "invalid_arguments",
            format!(
                "The message is {} bytes; the limit is {MAX_MESSAGE_BYTES}. Write the content to a file and send its path instead.",
                message.len()
            ),
            json!({"bytes": message.len(), "limit": MAX_MESSAGE_BYTES}),
        ));
    }
    Ok(())
}

/// Releases the per-task "send or cancel in progress" mark.
struct BusyGuard<'a> {
    manager: &'a TaskManager,
    id: String,
}

impl Drop for BusyGuard<'_> {
    fn drop(&mut self) {
        lock(self.manager.busy()).remove(&self.id);
    }
}

fn mark_busy<'a>(manager: &'a TaskManager, id: &str) -> Option<BusyGuard<'a>> {
    lock(manager.busy())
        .insert(id.to_owned())
        .then(|| BusyGuard {
            manager,
            id: id.to_owned(),
        })
}

fn pending_message(text: &str) -> PendingMessage {
    PendingMessage {
        id: uuid::Uuid::new_v4().simple().to_string(),
        text: text.to_owned(),
        created_at: now_ms(),
    }
}

fn cancelled_outcome(id: &str) -> Outcome {
    Outcome::new(
        "not_continuable",
        format!("Task {id} was cancelled and is not continued; spawn a new task instead."),
        json!({"id": id}),
    )
}

/// `task_send` (spec §1) for a task of `manager`'s state directory.
pub fn send(
    manager: &Arc<TaskManager>,
    caller: &Caller,
    to: &str,
    message: &str,
    all_sessions: bool,
) -> Outcome {
    if let Err(outcome) = check_message(message) {
        return outcome;
    }
    let Some(record) = resolve(manager, to) else {
        return not_found(manager, caller, to);
    };
    if let Some(denied) = scope_check(&record, caller, all_sessions) {
        return denied;
    }
    let id = record.id.clone();
    if lock(manager.cancelling()).contains_key(&id) || record.status == TaskStatus::Cancelled {
        return cancelled_outcome(&id);
    }
    let Some(_busy) = mark_busy(manager, &id) else {
        return Outcome::new(
            "not_continuable",
            format!("Another message or cancel for task {id} is in progress; retry in a moment."),
            json!({"id": id}),
        );
    };
    match record.status {
        TaskStatus::Cancelled => cancelled_outcome(&id),
        TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Interrupted => {
            match manager.resume(&id, Some(message), true) {
                Ok(resumed) => Outcome::new(
                    "revived",
                    format!(
                        "Task {id} continues in its own session with the message (run {}); its result is delivered when it finishes.",
                        resumed.runs
                    ),
                    json!({"id": id, "run": resumed.runs, "session_id": resumed.session_id}),
                ),
                Err(error) => Outcome::new(
                    "not_continuable",
                    format!("Task {id} could not be continued: {error}"),
                    json!({"id": id}),
                ),
            }
        }
        TaskStatus::Queued | TaskStatus::Running => {
            if record.status == TaskStatus::Running
                && let Some(outcome) = try_steer(manager, caller, &id, message)
            {
                return outcome;
            }
            match queue(manager, &id, message) {
                Some(outcome) => outcome,
                // The run settled between the lookup and the write: continue it instead.
                None => match manager.resume(&id, Some(message), true) {
                    Ok(resumed) => Outcome::new(
                        "revived",
                        format!(
                            "Task {id} had just finished; it continues with the message (run {}).",
                            resumed.runs
                        ),
                        json!({"id": id, "run": resumed.runs, "session_id": resumed.session_id}),
                    ),
                    Err(error) => Outcome::new(
                        "not_continuable",
                        format!("Task {id} changed state meanwhile ({error}); retry the message."),
                        json!({"id": id}),
                    ),
                },
            }
        }
    }
}

/// Splits a team target `<team>/<member>` (or `<team>/*`). Task ids never contain `/`.
pub fn split_team_target(to: &str) -> Option<(&str, &str)> {
    let (team, member) = to.trim().split_once('/')?;
    (!team.is_empty() && !member.is_empty()).then_some((team, member))
}

/// Writes a steering delivery when the child has a live channel (spec §1.1).
fn try_steer(manager: &TaskManager, caller: &Caller, id: &str, message: &str) -> Option<Outcome> {
    let channel = manager.steer_channel()?;
    // Held while writing, so a run that settles meanwhile sees the delivery and reclaims it.
    let mut steers = lock(manager.steers());
    let record = manager.state().load(id)?;
    if record.status != TaskStatus::Running {
        return None;
    }
    let path = channel.steer(&record.session_id, &caller.session_id, message)?;
    if manager.live_here(id) {
        steers.entry(id.to_owned()).or_default().push(WrittenSteer {
            path,
            message: pending_message(message),
        });
    }
    Some(Outcome::new(
        "steered",
        format!("Delivered the message into the current turn of task {id}."),
        json!({"id": id}),
    ))
}

/// Appends to the pending list; `None` when the task is no longer active.
fn queue(manager: &TaskManager, id: &str, message: &str) -> Option<Outcome> {
    let result = manager.update(id, |record| {
        if !record.status.is_active() {
            return None;
        }
        record.pending_messages.push(pending_message(message));
        Some((record.status, record.pending_messages.len()))
    });
    Some(match result {
        Ok((_, Some((status, position)))) => {
            let when = if status == TaskStatus::Queued {
                "with its first run"
            } else {
                "as a follow-up run when the current run finishes"
            };
            Outcome::new(
                "queued",
                format!("Queued the message for task {id} (position {position}); it is delivered {when}."),
                json!({"id": id, "position": position, "status": status.as_str()}),
            )
        }
        Ok((record, None)) if record.status == TaskStatus::Cancelled => cancelled_outcome(id),
        Ok((_, None)) => return None,
        Err(error) => Outcome::new(
            "not_continuable",
            format!("Task {id}: {error}; retry the message."),
            json!({"id": id}),
        ),
    })
}

/// `task_cancel` (spec §2): `cancelled`, `noop` or `not_found`.
pub async fn cancel(
    manager: &Arc<TaskManager>,
    caller: &Caller,
    id_or_name: &str,
    reason: Option<&str>,
) -> Outcome {
    let Some(record) = resolve(manager, id_or_name) else {
        return not_found(manager, caller, id_or_name);
    };
    let id = record.id.clone();
    let reason = reason.map(str::trim).filter(|r| !r.is_empty()).map(str::to_owned);
    let previous = record.status;
    if let Some(token) = manager.cancel_token(&id) {
        // Queued or running here: stop the run; `finish` records `cancelled` with the reason and
        // drops the pending list.
        // Steers the child has not drained yet are dropped with the pending list.
        let dropped = record.pending_messages.len()
            + lock(manager.steers())
                .get(&id)
                .map_or(0, |steers| steers.iter().filter(|steer| steer.path.exists()).count());
        lock(manager.cancelling()).insert(id.clone(), reason.clone());
        token.cancel();
        let settled = manager.wait(&id, Some(CANCEL_SETTLE)).await;
        let status = settled.map_or(TaskStatus::Cancelled, |r| r.status);
        if !manager.live_here(&id) {
            // Normally removed when the run ends; this covers a run that ended just before the
            // cancel was recorded.
            lock(manager.cancelling()).remove(&id);
        }
        if status.is_active() || status == TaskStatus::Cancelled {
            return cancelled_result(&id, previous, TaskStatus::Cancelled, dropped, false);
        }
        return Outcome::new(
            "noop",
            format!("Task {id} finished as {} before the cancel took effect.", status.as_str()),
            json!({"id": id, "status": status.as_str()}),
        );
    }
    match previous {
        TaskStatus::Cancelled => Outcome::new(
            "noop",
            format!("Task {id} is already cancelled."),
            json!({"id": id, "status": "cancelled"}),
        ),
        TaskStatus::Completed | TaskStatus::Failed => Outcome::new(
            "noop",
            format!("Task {id} is {}; nothing to cancel.", previous.as_str()),
            json!({"id": id, "status": previous.as_str()}),
        ),
        TaskStatus::Queued | TaskStatus::Running | TaskStatus::Interrupted => {
            // Owned by another process (running) or by none (interrupted).
            let signalled = match record.pid.filter(|pid| previous == TaskStatus::Running && pid_alive(*pid)) {
                Some(pid) => {
                    crate::manager::terminate(pid);
                    true
                }
                None => false,
            };
            let result = manager.update(&id, |stored| {
                let dropped = stored.pending_messages.len();
                stored.pending_messages.clear();
                stored.status = TaskStatus::Cancelled;
                stored.error.clone_from(&reason);
                stored.finished_at_ms = Some(now_ms());
                dropped
            });
            match result {
                Ok((_, dropped)) => {
                    cancelled_result(&id, previous, TaskStatus::Cancelled, dropped, signalled)
                }
                Err(error) => Outcome::new(
                    "noop",
                    format!("Task {id} could not be cancelled: {error}"),
                    json!({"id": id}),
                ),
            }
        }
    }
}

fn cancelled_result(
    id: &str,
    previous: TaskStatus,
    status: TaskStatus,
    dropped: usize,
    signalled: bool,
) -> Outcome {
    let mut text = format!(
        "Task {id}: {} -> {}.",
        previous.as_str(),
        status.as_str()
    );
    if dropped > 0 {
        text.push_str(&format!(
            " {dropped} queued message{} not delivered.",
            if dropped == 1 { " was" } else { "s were" }
        ));
    }
    Outcome::new(
        "cancelled",
        text,
        json!({"id": id, "previous": previous.as_str(), "status": status.as_str(),
            "dropped_messages": dropped, "signalled": signalled}),
    )
}

/// The dialogs `/task-kill` needs.
pub trait KillUi: Send + Sync {
    fn has_ui(&self) -> bool;
    fn select<'a>(&'a self, title: &'a str, options: &'a [String]) -> BoxFuture<'a, Option<String>>;
    fn confirm<'a>(&'a self, title: &'a str, message: &'a str) -> BoxFuture<'a, bool>;
    fn notify(&self, message: &str, kind: NotificationType);
}

/// [`KillUi`] over an extension context.
pub struct ContextUi<'a>(pub &'a ExtensionContext);

impl KillUi for ContextUi<'_> {
    fn has_ui(&self) -> bool {
        self.0.has_ui
    }
    fn select<'a>(&'a self, title: &'a str, options: &'a [String]) -> BoxFuture<'a, Option<String>> {
        self.0.ui.select(title, options, ExtensionUiDialogOptions::default())
    }
    fn confirm<'a>(&'a self, title: &'a str, message: &'a str) -> BoxFuture<'a, bool> {
        self.0.ui.confirm(title, message, ExtensionUiDialogOptions::default())
    }
    fn notify(&self, message: &str, kind: NotificationType) {
        self.0.ui.notify(message, kind);
    }
}

/// The tasks `/task-kill` offers: this session's `queued`, `running` and `interrupted` ones.
pub fn kill_candidates(manager: &TaskManager, session_id: &str) -> Vec<TaskRecord> {
    manager
        .list()
        .into_iter()
        .filter(|record| {
            matches!(
                record.status,
                TaskStatus::Queued | TaskStatus::Running | TaskStatus::Interrupted
            ) && owned_by(record, session_id)
        })
        .collect()
}

/// `<name> (<id>) <status>`, or `<id> <status>` without a distinct name.
pub fn kill_label(record: &TaskRecord) -> String {
    match record.name.as_deref().filter(|name| *name != record.id) {
        Some(name) => format!("{name} ({}) {}", record.id, record.status.as_str()),
        None => format!("{} {}", record.id, record.status.as_str()),
    }
}

/// `/task-kill` (spec §3). Returns the id it cancelled, if any.
pub async fn task_kill(manager: &Arc<TaskManager>, caller: &Caller, ui: &dyn KillUi) -> Option<String> {
    let candidates = kill_candidates(manager, &caller.session_id);
    if candidates.is_empty() {
        ui.notify("No queued, running or interrupted task to cancel.", NotificationType::Info);
        return None;
    }
    let labels: Vec<String> = candidates.iter().map(kill_label).collect();
    if !ui.has_ui() {
        ui.notify(
            &format!(
                "Tasks that can be cancelled:\n{}\nUse task_cancel with the id to cancel one.",
                labels.join("\n")
            ),
            NotificationType::Info,
        );
        return None;
    }
    let chosen = ui.select("Cancel which task?", &labels).await?;
    let index = labels.iter().position(|label| *label == chosen)?;
    let id = candidates[index].id.clone();
    if !ui
        .confirm("Cancel task", &format!("Cancel task {id}? This cannot be undone."))
        .await
    {
        return None;
    }
    let outcome = cancel(manager, caller, &id, Some(KILL_REASON)).await;
    if outcome.kind == "cancelled" {
        ui.notify(&format!("Cancelled task {id}."), NotificationType::Info);
        Some(id)
    } else {
        ui.notify(&outcome.text, NotificationType::Error);
        None
    }
}

/// Whether a session receives gateway deliveries as a delegated run: a `-p`/JSON run started with
/// `MHC_TASK_ID` (spec §1.1). Long-lived sessions receive through the gateway extension itself.
pub fn delegated_receiver(mode: ExtensionMode, task_id: Option<&str>) -> bool {
    matches!(mode, ExtensionMode::Print | ExtensionMode::Json)
        && task_id.is_some_and(|id| !id.trim().is_empty())
}

/// The delegated run's endpoint record; removed by [`DelegatedEndpoint::close`] or on drop.
#[derive(Debug)]
pub struct DelegatedEndpoint {
    store: Store,
    session_id: String,
}

impl DelegatedEndpoint {
    /// Writes the endpoint record of `session_id` and creates its inbox.
    ///
    /// # Errors
    /// Filesystem errors or an invalid session id.
    pub fn register(store: Store, session_id: &str, cwd: &str) -> std::io::Result<Self> {
        store.register(&Endpoint {
            version: maho_gateway::store::VERSION,
            session_id: session_id.to_owned(),
            name: std::env::var(TASK_ID_ENV).ok(),
            cwd: cwd.to_owned(),
            pid: std::process::id(),
            started_at: maho_gateway::store::now(),
        })?;
        Ok(Self {
            store,
            session_id: session_id.to_owned(),
        })
    }
    pub fn store(&self) -> &Store {
        &self.store
    }
    pub fn session_id(&self) -> &str {
        &self.session_id
    }
    /// Removes the endpoint record.
    pub fn close(self) {}
}

impl Drop for DelegatedEndpoint {
    fn drop(&mut self) {
        self.store.unregister(&self.session_id);
    }
}

/// Sends one gateway delivery into the session; `Err` leaves it for the next pass.
pub type DeliverFn = Arc<dyn Fn(CustomMessage, SendMessageOptions) -> Result<(), String> + Send + Sync>;

/// A delegated run's gateway inbox (spec §1.1): the endpoint record plus a drain loop as in
/// `gateway.md` §5. Dropping it stops the loop and removes the record.
pub struct DelegatedInbox {
    endpoint: DelegatedEndpoint,
    stop: Arc<AtomicBool>,
}

impl std::fmt::Debug for DelegatedInbox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DelegatedInbox")
            .field("session_id", &self.endpoint.session_id)
            .finish_non_exhaustive()
    }
}

impl DelegatedInbox {
    /// Registers `session_id` and drains its inbox every `poll` on a helper thread.
    ///
    /// # Errors
    /// The endpoint cannot be written or the thread cannot start.
    pub fn start(
        store: Store,
        session_id: &str,
        cwd: &str,
        poll: Duration,
        deliver: DeliverFn,
    ) -> std::io::Result<Self> {
        let endpoint = DelegatedEndpoint::register(store.clone(), session_id, cwd)?;
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let session = session_id.to_owned();
        let handle = tokio::runtime::Handle::try_current().ok();
        let _poller = std::thread::Builder::new()
            .name("mhc-task-inbox".into())
            .spawn(move || {
                let _entered = handle.as_ref().map(tokio::runtime::Handle::enter);
                let receiver = maho_gateway::Receiver::new();
                let send = |message: CustomMessage, options: SendMessageOptions| {
                    deliver(message, options)
                };
                while !flag.load(Ordering::SeqCst) {
                    receiver.poll_once(&store, &session, &send);
                    std::thread::sleep(poll);
                }
            })?;
        Ok(Self { endpoint, stop })
    }

    pub fn endpoint(&self) -> &DelegatedEndpoint {
        &self.endpoint
    }
}

impl Drop for DelegatedInbox {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}
