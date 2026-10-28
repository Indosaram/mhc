//! `session_relocation` (senpi `modes/rpc/session-relocation.ts`, docs/rpc.md "Continuing a session on another
//! computer"): the session asks the connection that offers relocation with `host_request`, only that connection's
//! `host_response` counts, and an accepted move is announced to it with `relocation_due` once the run that asked for
//! it settles. The engine never interrupts that run and never moves anything itself: the host runs the existing
//! `release_session` reason `move` and `move_session` steps.
use crate::session_relocation_channel::{HostResponseOutcome, RelocationChannel, RelocationRecordKind};
use crate::session_release_interrupt::{BoxFuture, SubscriptionGuard};
use maho_core::session_move::bundle::is_plain_id;
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

/// Longest the session waits for the host's answer before refusing the request itself.
pub const HOST_ANSWER_TIMEOUT_MS: u64 = 60_000;

/// Why no host can be asked: none offers relocation (`not_offered`), or this session cannot be handed over
/// (`not_releasable`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RelocationUnavailable {
    pub reason: &'static str,
    pub detail: &'static str,
}

/// `RELOCATION_NOT_OFFERED`.
pub const RELOCATION_NOT_OFFERED: RelocationUnavailable = RelocationUnavailable {
    reason: "not_offered",
    detail: "no app connected to this session offers moving it to another computer",
};

/// `RELOCATION_NOT_RELEASABLE`: the session has no file to move.
pub const RELOCATION_NOT_RELEASABLE: RelocationUnavailable = RelocationUnavailable {
    reason: "not_releasable",
    detail: "this session is not saved to a file",
};

/// A request that reached no move: the host declined it (its own reason), or the engine did before asking.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelocationRefused {
    pub reason: String,
    pub detail: String,
}

fn refused(reason: &str, detail: &str) -> RelocationRefused {
    RelocationRefused {
        reason: reason.to_owned(),
        detail: detail.to_owned(),
    }
}

/// One computer the host offers; the agent names it by `name`. Hints are the host's, passed through unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelocationTarget {
    pub name: String,
    pub online: bool,
    /// Free-form capacity hint from the host, for example `high` or `busy`.
    pub capacity: Option<String>,
    /// The host expects this computer to sleep soon.
    pub about_to_sleep: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelocationTargetsResult {
    Listed(Vec<RelocationTarget>),
    Unavailable(RelocationUnavailable),
    Refused(RelocationRefused),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelocationRequestResult {
    /// The host accepted; the move starts when the current turn ends, never before.
    Scheduled { target: String, move_id: String },
    Unavailable(RelocationUnavailable),
    Refused(RelocationRefused),
}

/// What the agent asks for: a computer it names, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelocationRequest {
    pub target: String,
    pub reason: String,
}

/// The run events relocation watches: a user-stopped run takes its scheduled move with it, and settlement is when an
/// accepted move is announced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelocationRunEvent {
    AgentEnd,
    AgentSettled,
    Other,
}

/// The engine's stand-in for an `AbortSignal`: `true` once the caller withdrew the request.
pub type CancelSignal = tokio::sync::watch::Receiver<bool>;

/// One listener of a session's run events.
pub type RunListener = Arc<dyn Fn(RelocationRunEvent) + Send + Sync>;

/// `prompt` calls not yet returned (senpi `pendingPrompts()`).
pub type PendingPrompts = Arc<dyn Fn() -> Vec<BoxFuture<'static, ()>> + Send + Sync>;

/// The slice of an `AgentSession` a relocation reads and drives (senpi reads the `AgentSession` directly).
pub trait RelocationSession: Send + Sync {
    /// `sessionFile`: a session with no file cannot be released for a move.
    fn session_file(&self) -> Option<String>;
    fn is_streaming(&self) -> bool;
    /// `currentAbortSource === "user"`.
    fn stopped_by_user(&self) -> bool;
    /// `sendCustomMessage(message, { triggerTurn })`.
    fn send_custom_message(&self, message: Value, trigger_turn: bool) -> BoxFuture<'static, ()>;
    /// Calls `listener` on every run event until the guard is dropped.
    fn subscribe(&self, listener: RunListener) -> SubscriptionGuard;
}

/// What a host lends a session's relocation: the session, the prompt calls behind it, and the one channel it may ask.
pub struct RpcSessionRelocationDeps {
    pub session: Arc<dyn RelocationSession>,
    /// `prompt` calls not yet returned; `release_session` counts them as a running turn.
    pub pending_prompts: PendingPrompts,
    pub channel: Arc<RelocationChannel>,
}

/// Why a scheduled move was withdrawn: the accepting app left, the user stopped the run, or the app stopped offering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DropReason {
    Disconnected,
    Aborted,
    Withdrawn,
}

impl DropReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::Disconnected => "disconnected",
            Self::Aborted => "aborted",
            Self::Withdrawn => "withdrawn",
        }
    }
}

/// A move the host accepted and the engine has not announced yet.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Scheduled {
    move_id: String,
    target: String,
    reason: String,
    connection: String,
    drop_reason: Option<DropReason>,
}

/// One `host_response` that may settle `pending`: only its own connection may answer it.
struct Pending {
    connection: String,
    finish: tokio::sync::oneshot::Sender<Answer>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Answer {
    Data(Option<Value>),
    Refused(RelocationRefused),
}

/// The one question a relocation asks the offering connection.
enum HostAsk {
    ListTargets,
    Relocate { target: String, reason: String },
}

impl HostAsk {
    fn into_record(self, id: &str) -> Value {
        match self {
            Self::ListTargets => {
                json!({"type":"host_request","id":id,"method":"list_relocation_targets"})
            }
            Self::Relocate { target, reason } => json!({
                "type":"host_request","id":id,"method":"relocate_session","target":target,"reason":reason
            }),
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// `record(value)`: a plain object, copied so later edits cannot change the answer being read.
fn record(value: &Value) -> Option<Map<String, Value>> {
    value.as_object().cloned()
}

fn parse_target(value: &Value) -> Option<RelocationTarget> {
    let fields = value.as_object()?;
    let name = fields
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())?;
    let online = fields.get("online").and_then(Value::as_bool)?;
    Some(RelocationTarget {
        name: name.to_owned(),
        online,
        capacity: fields
            .get("capacity")
            .and_then(Value::as_str)
            .map(str::to_owned),
        about_to_sleep: fields.get("aboutToSleep").and_then(Value::as_bool),
    })
}

fn cancelled(signal: Option<&CancelSignal>) -> bool {
    signal.is_some_and(|signal| *signal.borrow())
}

/// Resolves once the caller withdrew the request, and never when it lent no signal.
async fn wait_cancel(signal: Option<CancelSignal>) {
    let Some(mut signal) = signal else {
        std::future::pending::<()>().await;
        return;
    };
    loop {
        if *signal.borrow_and_update() {
            return;
        }
        if signal.changed().await.is_err() {
            std::future::pending::<()>().await;
            return;
        }
    }
}

/// Holds `requesting` for as long as one `request` call is in flight, so a dropped caller cannot wedge it.
struct RequestingGuard<'a>(&'a AtomicBool);

impl Drop for RequestingGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// The host-mediated half of `session_relocation`: it asks, waits, and announces; it never moves anything.
pub struct RpcSessionRelocation {
    deps: RpcSessionRelocationDeps,
    pending: Mutex<BTreeMap<String, Pending>>,
    scheduled: Mutex<Option<Scheduled>>,
    requesting: AtomicBool,
    timeout: std::time::Duration,
    unsubscribe: Mutex<Option<SubscriptionGuard>>,
}

impl RpcSessionRelocation {
    /// Builds the host and subscribes it to the session's run events (senpi's constructor).
    pub fn new(deps: RpcSessionRelocationDeps) -> Arc<Self> {
        let host = Arc::new(Self {
            deps,
            pending: Mutex::new(BTreeMap::new()),
            scheduled: Mutex::new(None),
            requesting: AtomicBool::new(false),
            timeout: std::time::Duration::from_millis(HOST_ANSWER_TIMEOUT_MS),
            unsubscribe: Mutex::new(None),
        });
        let weak = Arc::downgrade(&host);
        let listener: RunListener = Arc::new(move |event| {
            if let Some(host) = weak.upgrade() {
                host.observe(event);
            }
        });
        let guard = host.deps.session.subscribe(listener);
        *lock(&host.unsubscribe) = Some(guard);
        host
    }

    /// Whether a host offers relocation for this session right now; decides if the agent sees the tools at all.
    pub fn offered(&self) -> bool {
        self.deps.channel.offerer().is_some()
    }

    /// The computers the offering connection offers, or why there are none.
    pub async fn targets(&self, cancel: Option<CancelSignal>) -> RelocationTargetsResult {
        let Some(connection) = self.deps.channel.offerer() else {
            return RelocationTargetsResult::Unavailable(RELOCATION_NOT_OFFERED);
        };
        let data = match self.ask(HostAsk::ListTargets, &connection, cancel).await {
            Answer::Data(data) => data,
            Answer::Refused(denial) => return RelocationTargetsResult::Refused(denial),
        };
        let raw = data
            .as_ref()
            .and_then(record)
            .and_then(|fields| fields.get("targets").cloned());
        let targets = raw
            .as_ref()
            .and_then(Value::as_array)
            .map(|items| items.iter().map(parse_target).collect::<Vec<_>>());
        match targets {
            Some(targets) if targets.iter().all(Option::is_some) => {
                RelocationTargetsResult::Listed(targets.into_iter().flatten().collect())
            }
            _ => RelocationTargetsResult::Refused(refused(
                "invalid_host_answer",
                "the app sent a list of computers this session cannot read",
            )),
        }
    }

    /// Asks the offering connection to move this session; an accepted move is announced once the run settles.
    pub async fn request(
        &self,
        request: RelocationRequest,
        cancel: Option<CancelSignal>,
    ) -> RelocationRequestResult {
        let Some(connection) = self.deps.channel.offerer() else {
            return RelocationRequestResult::Unavailable(RELOCATION_NOT_OFFERED);
        };
        if self.deps.session.session_file().is_none() {
            return RelocationRequestResult::Unavailable(RELOCATION_NOT_RELEASABLE);
        }
        if lock(&self.scheduled).is_some() || self.requesting.load(Ordering::SeqCst) {
            let target = lock(&self.scheduled)
                .as_ref()
                .map_or_else(|| "another computer".to_owned(), |due| due.target.clone());
            return RelocationRequestResult::Refused(refused(
                "already_scheduled",
                &format!("a move to {target} is already set for the end of this turn"),
            ));
        }
        self.requesting.store(true, Ordering::SeqCst);
        let _requesting = RequestingGuard(&self.requesting);
        let answer = self
            .ask(
                HostAsk::Relocate {
                    target: request.target.clone(),
                    reason: request.reason.clone(),
                },
                &connection,
                cancel,
            )
            .await;
        let data = match answer {
            Answer::Data(data) => data,
            Answer::Refused(denial) => return RelocationRequestResult::Refused(denial),
        };
        self.settle_request(&request, data.as_ref().and_then(record), &connection)
    }

    /// Settles one `host_response`; only the connection the request went to may answer it.
    pub fn respond(&self, frame: &Value) -> HostResponseOutcome {
        let Some(id) = frame.get("id").and_then(Value::as_str) else {
            return HostResponseOutcome::Unknown;
        };
        let Some(pending) = lock(&self.pending).remove(id) else {
            return HostResponseOutcome::Unknown;
        };
        if self.deps.channel.current().as_deref() != Some(pending.connection.as_str()) {
            // Another connection's answer does not count, and the request stays open for the one that was asked.
            lock(&self.pending).insert(id.to_owned(), pending);
            return HostResponseOutcome::NotOfferingConnection;
        }
        let answer = match frame.get("error").and_then(Value::as_str) {
            Some(error) => Answer::Refused(refused("host_error", error)),
            None => Answer::Data(frame.get("data").cloned()),
        };
        let _ = pending.finish.send(answer);
        HostResponseOutcome::Answered
    }

    /// The connection `connection_id` left the session: what it accepted or was asked ends here, never later.
    pub fn connection_detached(&self, connection_id: &str) {
        let accepted_by_it = lock(&self.scheduled).as_ref().is_some_and(|due| {
            due.connection == connection_id && due.drop_reason.is_none()
        });
        if accepted_by_it {
            self.drop_scheduled(DropReason::Disconnected);
        }
        let waiting: Vec<Pending> = {
            let mut pending = lock(&self.pending);
            let ids: Vec<String> = pending
                .iter()
                .filter(|(_, request)| request.connection == connection_id)
                .map(|(id, _)| id.clone())
                .collect();
            ids.into_iter()
                .filter_map(|id| pending.remove(&id))
                .collect()
        };
        for pending in waiting {
            let _ = pending.finish.send(Answer::Refused(refused(
                "app_disconnected",
                "the app that was asked left this session",
            )));
        }
    }

    /// Drops the subscription and refuses everything still waiting: the session is gone.
    pub fn dispose(&self) {
        drop(lock(&self.unsubscribe).take());
        *lock(&self.scheduled) = None;
        let waiting: Vec<Pending> = std::mem::take(&mut *lock(&self.pending)).into_values().collect();
        for pending in waiting {
            let _ = pending.finish.send(Answer::Refused(refused(
                "session_closed",
                "this session was closed",
            )));
        }
    }

    /// A run event the session published, in the order the session published it.
    fn observe(self: &Arc<Self>, event: RelocationRunEvent) {
        match event {
            RelocationRunEvent::AgentEnd => {
                // A run the user stopped takes its scheduled move with it: the user chose to stay here.
                if self.deps.session.stopped_by_user()
                    && let Some(due) = lock(&self.scheduled).as_mut()
                {
                    due.drop_reason = Some(DropReason::Aborted);
                }
            }
            RelocationRunEvent::AgentSettled => {
                let due = lock(&self.scheduled).clone();
                let Some(due) = due else {
                    return;
                };
                match due.drop_reason {
                    // agent_end still streams: wait until settlement so the correction is appended, not parked.
                    Some(reason) => self.drop_scheduled(reason),
                    None => {
                        let host = Arc::clone(self);
                        tokio::spawn(async move { host.announce_when_releasable().await });
                    }
                }
            }
            RelocationRunEvent::Other => {}
        }
    }

    /// After the run settled, waits out the prompt calls behind it, so a `release_session` answering the notice finds
    /// the session quiet.
    async fn announce_when_releasable(&self) {
        loop {
            let calls = (self.deps.pending_prompts)();
            if calls.is_empty() {
                break;
            }
            for call in calls {
                let _ = call.await;
            }
        }
        let Some(due) = lock(&self.scheduled).clone() else {
            return;
        };
        // A new run took over meanwhile: its own settle announces the move.
        if self.deps.session.is_streaming() {
            return;
        }
        // Only the connection that accepted, while it still offers relocation here, is told to move the session.
        if self.deps.channel.offerer().as_deref() != Some(due.connection.as_str()) {
            self.drop_scheduled(DropReason::Withdrawn);
            return;
        }
        *lock(&self.scheduled) = None;
        self.deps.channel.send(
            &due.connection,
            json!({
                "type":"relocation_due","moveId":due.move_id,"target":due.target,"reason":due.reason
            }),
            RelocationRecordKind::Event,
        );
    }

    fn settle_request(
        &self,
        request: &RelocationRequest,
        data: Option<Map<String, Value>>,
        connection: &str,
    ) -> RelocationRequestResult {
        let accepted = data
            .as_ref()
            .and_then(|fields| fields.get("accepted"))
            .and_then(Value::as_bool);
        if accepted == Some(false) {
            let text = |key: &str| {
                data.as_ref()
                    .and_then(|fields| fields.get(key))
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty())
                    .map(str::to_owned)
            };
            return RelocationRequestResult::Refused(refused(
                text("reason").as_deref().unwrap_or("declined"),
                text("detail")
                    .as_deref()
                    .unwrap_or("the app declined the move"),
            ));
        }
        let move_id = data
            .as_ref()
            .and_then(|fields| fields.get("moveId"))
            .filter(|value| is_plain_id(value))
            .and_then(Value::as_str)
            .map(str::to_owned);
        let Some(move_id) = move_id.filter(|_| accepted == Some(true)) else {
            return RelocationRequestResult::Refused(refused(
                "invalid_host_answer",
                "the app accepted the move without a usable move id",
            ));
        };
        let target = data
            .as_ref()
            .and_then(|fields| fields.get("target"))
            .and_then(Value::as_str)
            .filter(|target| !target.is_empty())
            .map_or_else(|| request.target.clone(), str::to_owned);
        *lock(&self.scheduled) = Some(Scheduled {
            move_id: move_id.clone(),
            target: target.clone(),
            reason: request.reason.clone(),
            connection: connection.to_owned(),
            drop_reason: None,
        });
        RelocationRequestResult::Scheduled { target, move_id }
    }

    /// Tells the session's own transcript why the move is off, and keeps the session working here.
    fn drop_scheduled(&self, reason: DropReason) {
        let Some(due) = lock(&self.scheduled).take() else {
            return;
        };
        let content = format!(
            "The session will not continue on {} ({}). The session stays on this computer; keep working here.",
            due.target,
            reason.as_str()
        );
        let message = json!({
            "customType":"session_relocation",
            "content":content,
            "display":false,
            "details":{"status":"dropped","reason":reason.as_str(),"moveId":due.move_id,"target":due.target}
        });
        let send = self.deps.session.send_custom_message(message, false);
        tokio::spawn(send);
    }

    fn ask(
        &self,
        fields: HostAsk,
        connection: &str,
        cancel: Option<CancelSignal>,
    ) -> BoxFuture<'_, Answer> {
        let connection = connection.to_owned();
        Box::pin(async move {
            if cancelled(cancel.as_ref()) {
                return Answer::Refused(refused("cancelled", "the request was cancelled"));
            }
            let id = format!("hr_{}", uuid::Uuid::new_v4());
            let (finish, answered) = tokio::sync::oneshot::channel();
            lock(&self.pending).insert(
                id.clone(),
                Pending {
                    connection: connection.clone(),
                    finish,
                },
            );
            self.deps
                .channel
                .send(&connection, fields.into_record(&id), RelocationRecordKind::Request);
            let answer = tokio::select! {
                biased;
                answer = answered => answer.unwrap_or_else(|_| Answer::Refused(refused("session_closed", "this session was closed"))),
                () = wait_cancel(cancel) => Answer::Refused(refused("cancelled", "the request was cancelled")),
                () = tokio::time::sleep(self.timeout) => Answer::Refused(refused("no_answer", "the app did not answer in time")),
            };
            lock(&self.pending).remove(&id);
            answer
        })
    }
}

/// The slice of a live `AgentSession` a relocation reads and drives (senpi reads the `AgentSession` directly).
pub struct AgentSessionRelocation {
    session: maho_core::agent_session::AgentSession,
}

impl AgentSessionRelocation {
    pub fn new(session: maho_core::agent_session::AgentSession) -> Self {
        Self { session }
    }
}

impl RelocationSession for AgentSessionRelocation {
    fn session_file(&self) -> Option<String> {
        self.session.session_file()
    }

    fn is_streaming(&self) -> bool {
        self.session.is_streaming()
    }

    fn stopped_by_user(&self) -> bool {
        self.session.current_abort_source() == Some(maho_ext_api::AbortSource::User)
    }

    fn send_custom_message(&self, message: Value, trigger_turn: bool) -> BoxFuture<'static, ()> {
        let text = message
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let custom_type = message
            .get("customType")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let display = message
            .get("display")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let details = message.get("details").cloned();
        let session = self.session.clone();
        Box::pin(async move {
            let _ = session
                .send_custom_message(
                    maho_ext_api::CustomMessage {
                        custom_type,
                        content: vec![maho_ext_api::ToolContent::text(text)],
                        display,
                        details,
                    },
                    maho_ext_api::SendMessageOptions {
                        trigger_turn,
                        deliver_as: None,
                    },
                )
                .await;
        })
    }

    fn subscribe(&self, listener: RunListener) -> SubscriptionGuard {
        let subscription = self
            .session
            .subscribe(Arc::new(move |event: &maho_ext_api::AgentSessionEvent| {
                let run = match event {
                    maho_ext_api::AgentSessionEvent::AgentEnd { .. } => RelocationRunEvent::AgentEnd,
                    maho_ext_api::AgentSessionEvent::AgentSettled => RelocationRunEvent::AgentSettled,
                    _ => RelocationRunEvent::Other,
                };
                if run != RelocationRunEvent::Other {
                    listener(run);
                }
            }));
        Box::new(subscription)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_relocation_channel::{
        OutputLane, RelocationChannelOptions, RelocationClientInfo, SOLE_CONNECTION, relocation_channel,
    };
    use std::sync::atomic::AtomicUsize;

    /// The session a relocation speaks for, with the run events a real session publishes.
    #[derive(Default)]
    struct FakeSession {
        session_file: Mutex<Option<String>>,
        streaming: AtomicBool,
        stopped_by_user: AtomicBool,
        messages: Mutex<Vec<Value>>,
        listeners: Mutex<Vec<RunListener>>,
        messaged: tokio::sync::Notify,
    }

    impl FakeSession {
        fn saved() -> Arc<Self> {
            let session = Arc::new(Self::default());
            *session.session_file.lock().unwrap() = Some("/s.jsonl".to_owned());
            session
        }

        fn publish(&self, event: RelocationRunEvent) {
            for listener in self.listeners.lock().unwrap().iter() {
                listener(event);
            }
        }
    }

    impl RelocationSession for FakeSession {
        fn session_file(&self) -> Option<String> {
            self.session_file.lock().unwrap().clone()
        }

        fn is_streaming(&self) -> bool {
            self.streaming.load(Ordering::SeqCst)
        }

        fn stopped_by_user(&self) -> bool {
            self.stopped_by_user.load(Ordering::SeqCst)
        }

        fn send_custom_message(&self, message: Value, trigger_turn: bool) -> BoxFuture<'static, ()> {
            assert!(!trigger_turn, "a dropped move never starts a turn");
            self.messages.lock().unwrap().push(message);
            self.messaged.notify_one();
            Box::pin(async {})
        }

        fn subscribe(&self, listener: RunListener) -> SubscriptionGuard {
            self.listeners.lock().unwrap().push(listener);
            Box::new(())
        }
    }

    /// What this connection answers one `host_request` with; `None` leaves it unanswered.
    type AnswerSource = Box<dyn Fn(&Value) -> Option<Value> + Send + Sync>;

    /// The offering connection's side of the wire: what the session asked, what it sent as events, and how it answers.
    struct Wire {
        requests: Mutex<Vec<Value>>,
        events: Mutex<Vec<Value>>,
        answer: Mutex<Option<AnswerSource>>,
        host: Mutex<Option<std::sync::Weak<RpcSessionRelocation>>>,
        answered: AtomicUsize,
        announced: tokio::sync::Notify,
    }

    impl Default for Wire {
        fn default() -> Self {
            Self {
                requests: Mutex::new(Vec::new()),
                events: Mutex::new(Vec::new()),
                answer: Mutex::new(None),
                host: Mutex::new(None),
                answered: AtomicUsize::new(0),
                announced: tokio::sync::Notify::new(),
            }
        }
    }

    impl Wire {
        /// What this connection answers each `host_request` with; `None` leaves the request unanswered.
        fn answer_with(&self, answer: impl Fn(&Value) -> Option<Value> + Send + Sync + 'static) {
            *self.answer.lock().unwrap() = Some(Box::new(answer));
        }

        fn deliver(&self, record: Value) {
            self.requests.lock().unwrap().push(record.clone());
            self.answered.fetch_add(1, Ordering::SeqCst);
            let reply = {
                let answer = self.answer.lock().unwrap();
                answer
                    .as_ref()
                    .and_then(|answer| answer(&record))
                    .map(|data| json!({"type":"host_response","id":record["id"].clone(),"data":data}))
            };
            let Some(reply) = reply else {
                return;
            };
            let host = self
                .host
                .lock()
                .unwrap()
                .as_ref()
                .and_then(std::sync::Weak::upgrade);
            if let Some(host) = host {
                assert_eq!(host.respond(&reply), HostResponseOutcome::Answered);
            }
        }

        fn publish_event(&self, record: Value) {
            self.events.lock().unwrap().push(record);
            self.announced.notify_one();
        }
    }

    /// A classic single-connection handler: its own capabilities decide whether it offers relocation.
    fn setup(session: Arc<FakeSession>, capabilities: Option<Vec<String>>) -> (Arc<RpcSessionRelocation>, Arc<Wire>) {
        let wire = Arc::new(Wire::default());
        let requests = Arc::clone(&wire);
        let output: OutputLane = Arc::new(move |record| requests.deliver(record));
        let events = Arc::clone(&wire);
        let output_event: OutputLane = Arc::new(move |record| events.publish_event(record));
        let channel = Arc::new(relocation_channel(RelocationChannelOptions {
            capabilities: Arc::new(move || capabilities.clone()),
            client_info: None,
            output,
            output_event,
        }));
        let pending_prompts: PendingPrompts = Arc::new(Vec::new);
        let dep: Arc<dyn RelocationSession> = session;
        let host = RpcSessionRelocation::new(RpcSessionRelocationDeps {
            session: dep,
            pending_prompts,
            channel,
        });
        *wire.host.lock().unwrap() = Some(Arc::downgrade(&host));
        (host, wire)
    }

    fn offering() -> Vec<String> {
        vec![crate::custom_capability::SESSION_RELOCATION_CAPABILITY.to_owned()]
    }

    fn move_request() -> RelocationRequest {
        RelocationRequest {
            target: "laptop".to_owned(),
            reason: "the user is leaving".to_owned(),
        }
    }

    fn accepted() -> impl Fn(&Value) -> Option<Value> + Send + Sync + 'static {
        |_: &Value| Some(json!({"accepted":true,"moveId":"m-1"}))
    }

    #[tokio::test]
    async fn refuses_every_call_when_no_connection_offers_relocation() {
        let (host, wire) = setup(FakeSession::saved(), None);
        assert!(!host.offered());
        assert_eq!(
            host.targets(None).await,
            RelocationTargetsResult::Unavailable(RELOCATION_NOT_OFFERED)
        );
        assert_eq!(
            host.request(move_request(), None).await,
            RelocationRequestResult::Unavailable(RELOCATION_NOT_OFFERED)
        );
        assert!(wire.requests.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn refuses_a_move_of_a_session_that_was_never_saved() {
        let (host, wire) = setup(Arc::new(FakeSession::default()), Some(offering()));
        assert!(host.offered());
        assert_eq!(
            host.request(move_request(), None).await,
            RelocationRequestResult::Unavailable(RELOCATION_NOT_RELEASABLE)
        );
        assert!(wire.requests.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn lists_the_computers_the_offering_connection_sends() {
        let (host, wire) = setup(FakeSession::saved(), Some(offering()));
        wire.answer_with(|_: &Value| {
            Some(json!({"targets":[
                {"name":"laptop","online":true,"capacity":"high"},
                {"name":"desk","online":false,"aboutToSleep":true}
            ]}))
        });
        assert_eq!(
            host.targets(None).await,
            RelocationTargetsResult::Listed(vec![
                RelocationTarget {
                    name: "laptop".into(),
                    online: true,
                    capacity: Some("high".into()),
                    about_to_sleep: None,
                },
                RelocationTarget {
                    name: "desk".into(),
                    online: false,
                    capacity: None,
                    about_to_sleep: Some(true),
                },
            ])
        );
        let requests = wire.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0]["type"], json!("host_request"));
        assert_eq!(requests[0]["method"], json!("list_relocation_targets"));
        assert!(requests[0]["id"].as_str().is_some_and(|id| id.starts_with("hr_")));
    }

    #[tokio::test]
    async fn refuses_a_target_list_this_session_cannot_read() {
        for answer in [
            json!({}),
            json!({"targets":"laptop"}),
            json!({"targets":[{"name":"","online":true}]}),
            json!({"targets":[{"name":"laptop"}]}),
        ] {
            let (host, wire) = setup(FakeSession::saved(), Some(offering()));
            wire.answer_with(move |_: &Value| Some(answer.clone()));
            assert_eq!(
                host.targets(None).await,
                RelocationTargetsResult::Refused(RelocationRefused {
                    reason: "invalid_host_answer".into(),
                    detail: "the app sent a list of computers this session cannot read".into(),
                })
            );
        }
    }

    #[tokio::test]
    async fn schedules_an_accepted_move_and_announces_it_once_the_run_settles() {
        let session = FakeSession::saved();
        let (host, wire) = setup(Arc::clone(&session), Some(offering()));
        wire.answer_with(accepted());
        assert_eq!(
            host.request(move_request(), None).await,
            RelocationRequestResult::Scheduled {
                target: "laptop".into(),
                move_id: "m-1".into(),
            }
        );
        let requests = wire.requests.lock().unwrap().clone();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0]["method"], json!("relocate_session"));
        assert_eq!(requests[0]["target"], json!("laptop"));
        assert_eq!(requests[0]["reason"], json!("the user is leaving"));
        // agent_end alone announces nothing: the notice lands at settlement, never parked mid-run.
        session.publish(RelocationRunEvent::AgentEnd);
        assert!(wire.events.lock().unwrap().is_empty());
        session.publish(RelocationRunEvent::AgentSettled);
        tokio::time::timeout(std::time::Duration::from_secs(5), wire.announced.notified())
            .await
            .expect("the settlement announces the move");
        assert_eq!(
            wire.events.lock().unwrap().clone(),
            vec![json!({
                "type":"relocation_due","moveId":"m-1","target":"laptop","reason":"the user is leaving"
            })]
        );
    }

    #[tokio::test]
    async fn a_host_that_declines_names_its_own_reason() {
        let (host, wire) = setup(FakeSession::saved(), Some(offering()));
        wire.answer_with(|_: &Value| {
            Some(json!({"accepted":false,"reason":"offline","detail":"laptop is asleep"}))
        });
        assert_eq!(
            host.request(move_request(), None).await,
            RelocationRequestResult::Refused(RelocationRefused {
                reason: "offline".into(),
                detail: "laptop is asleep".into(),
            })
        );
        wire.answer_with(|_: &Value| Some(json!({"accepted":false})));
        assert_eq!(
            host.request(move_request(), None).await,
            RelocationRequestResult::Refused(RelocationRefused {
                reason: "declined".into(),
                detail: "the app declined the move".into(),
            })
        );
    }

    #[tokio::test]
    async fn an_acceptance_without_a_usable_move_id_is_refused() {
        for answer in [
            json!({"accepted":true}),
            json!({"accepted":true,"moveId":"../escape"}),
            json!({"accepted":"yes","moveId":"m-1"}),
        ] {
            let (host, wire) = setup(FakeSession::saved(), Some(offering()));
            wire.answer_with(move |_: &Value| Some(answer.clone()));
            assert_eq!(
                host.request(move_request(), None).await,
                RelocationRequestResult::Refused(RelocationRefused {
                    reason: "invalid_host_answer".into(),
                    detail: "the app accepted the move without a usable move id".into(),
                })
            );
        }
    }

    #[tokio::test]
    async fn one_scheduled_move_at_a_time() {
        let (host, wire) = setup(FakeSession::saved(), Some(offering()));
        wire.answer_with(accepted());
        assert!(matches!(
            host.request(move_request(), None).await,
            RelocationRequestResult::Scheduled { .. }
        ));
        assert_eq!(
            host.request(move_request(), None).await,
            RelocationRequestResult::Refused(RelocationRefused {
                reason: "already_scheduled".into(),
                detail: "a move to laptop is already set for the end of this turn".into(),
            })
        );
        assert_eq!(wire.answered.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_user_stopped_run_takes_its_scheduled_move_with_it() {
        let session = FakeSession::saved();
        let (host, wire) = setup(Arc::clone(&session), Some(offering()));
        wire.answer_with(accepted());
        assert!(matches!(
            host.request(move_request(), None).await,
            RelocationRequestResult::Scheduled { .. }
        ));
        session.stopped_by_user.store(true, Ordering::SeqCst);
        session.publish(RelocationRunEvent::AgentEnd);
        session.publish(RelocationRunEvent::AgentSettled);
        tokio::time::timeout(std::time::Duration::from_secs(5), session.messaged.notified())
            .await
            .expect("the dropped move is written to the session");
        assert!(wire.events.lock().unwrap().is_empty());
        let messages = session.messages.lock().unwrap().clone();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["customType"], json!("session_relocation"));
        assert_eq!(messages[0]["display"], json!(false));
        assert_eq!(
            messages[0]["content"],
            json!(
                "The session will not continue on laptop (aborted). The session stays on this computer; keep working here."
            )
        );
        assert_eq!(
            messages[0]["details"],
            json!({"status":"dropped","reason":"aborted","moveId":"m-1","target":"laptop"})
        );
    }

    #[tokio::test]
    async fn a_detached_connection_drops_the_move_and_refuses_what_it_was_asked() {
        let (host, wire) = setup(FakeSession::saved(), Some(offering()));
        // Unanswered: the ask is still pending when the app leaves.
        let mut asking = Box::pin(host.request(move_request(), None));
        assert!(futures::poll!(&mut asking).is_pending());
        assert_eq!(wire.requests.lock().unwrap().len(), 1);
        host.connection_detached(SOLE_CONNECTION);
        assert_eq!(
            asking.await,
            RelocationRequestResult::Refused(RelocationRefused {
                reason: "app_disconnected".into(),
                detail: "the app that was asked left this session".into(),
            })
        );

        // The same detachment after an acceptance: the move is dropped, once.
        let session = FakeSession::saved();
        let (host, wire) = setup(Arc::clone(&session), Some(offering()));
        wire.answer_with(accepted());
        assert!(matches!(
            host.request(move_request(), None).await,
            RelocationRequestResult::Scheduled { .. }
        ));
        host.connection_detached(SOLE_CONNECTION);
        let messages = session.messages.lock().unwrap().clone();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0]["details"]["reason"], json!("disconnected"));
        assert!(wire.events.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_cancelled_request_refuses_before_asking() {
        let (host, wire) = setup(FakeSession::saved(), Some(offering()));
        let (cancel, signal) = tokio::sync::watch::channel(true);
        drop(cancel);
        assert_eq!(
            host.request(move_request(), Some(signal)).await,
            RelocationRequestResult::Refused(RelocationRefused {
                reason: "cancelled".into(),
                detail: "the request was cancelled".into(),
            })
        );
        assert!(wire.requests.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn only_the_connection_that_was_asked_may_answer() {
        let wire = Arc::new(Wire::default());
        let requests = Arc::clone(&wire);
        let output: OutputLane = Arc::new(move |record| requests.deliver(record));
        let events = Arc::clone(&wire);
        let output_event: OutputLane = Arc::new(move |record| events.publish_event(record));
        let channel = Arc::new(relocation_channel(RelocationChannelOptions {
            capabilities: Arc::new(|| -> Option<Vec<String>> { None }),
            client_info: Some(RelocationClientInfo {
                connection_id: Arc::new(|| Some("peer-b".to_owned())),
                relocation_connection: Some(Arc::new(|| Some("peer-a".to_owned()))),
                deliver_to: Some(Arc::new(|_: &str, deliver: &(dyn Fn() + Send + Sync)| deliver())),
            }),
            output,
            output_event,
        }));
        assert_eq!(channel.offerer().as_deref(), Some("peer-a"));
        assert_eq!(channel.current().as_deref(), Some("peer-b"));
        let pending_prompts: PendingPrompts = Arc::new(Vec::new);
        let dep: Arc<dyn RelocationSession> = FakeSession::saved();
        let host = RpcSessionRelocation::new(RpcSessionRelocationDeps {
            session: dep,
            pending_prompts,
            channel,
        });
        let mut asking = Box::pin(host.request(move_request(), None));
        assert!(futures::poll!(&mut asking).is_pending());
        let asked = wire.requests.lock().unwrap()[0].clone();
        let frame = json!({"type":"host_response","id":asked["id"].clone(),"data":{"accepted":true,"moveId":"m-1"}});
        assert_eq!(host.respond(&frame), HostResponseOutcome::NotOfferingConnection);
        // The request stays open for the connection it went to, and the close refuses it.
        host.dispose();
        assert_eq!(
            asking.await,
            RelocationRequestResult::Refused(RelocationRefused {
                reason: "session_closed".into(),
                detail: "this session was closed".into(),
            })
        );
    }

    #[tokio::test(start_paused = true)]
    async fn an_unanswered_request_is_refused_once_the_answer_deadline_passes() {
        let (host, wire) = setup(FakeSession::saved(), Some(offering()));
        wire.answer_with(|_: &Value| -> Option<Value> { None });
        let mut asking = Box::pin(host.request(move_request(), None));
        assert!(futures::poll!(&mut asking).is_pending());
        tokio::time::advance(std::time::Duration::from_millis(HOST_ANSWER_TIMEOUT_MS)).await;
        assert_eq!(
            asking.await,
            RelocationRequestResult::Refused(RelocationRefused {
                reason: "no_answer".into(),
                detail: "the app did not answer in time".into(),
            })
        );
    }

    #[tokio::test]
    async fn a_connection_that_stops_offering_keeps_the_session_here() {
        let session = FakeSession::saved();
        let offered = Arc::new(Mutex::new(Some(offering())));
        let wire = Arc::new(Wire::default());
        let requests = Arc::clone(&wire);
        let output: OutputLane = Arc::new(move |record| requests.deliver(record));
        let events = Arc::clone(&wire);
        let output_event: OutputLane = Arc::new(move |record| events.publish_event(record));
        let capabilities = Arc::clone(&offered);
        let channel = Arc::new(relocation_channel(RelocationChannelOptions {
            capabilities: Arc::new(move || capabilities.lock().unwrap().clone()),
            client_info: None,
            output,
            output_event,
        }));
        let pending_prompts: PendingPrompts = Arc::new(Vec::new);
        let dep: Arc<dyn RelocationSession> = session.clone();
        let host = RpcSessionRelocation::new(RpcSessionRelocationDeps {
            session: dep,
            pending_prompts,
            channel,
        });
        *wire.host.lock().unwrap() = Some(Arc::downgrade(&host));
        wire.answer_with(accepted());
        assert!(matches!(
            host.request(move_request(), None).await,
            RelocationRequestResult::Scheduled { .. }
        ));
        *offered.lock().unwrap() = None;
        session.publish(RelocationRunEvent::AgentSettled);
        tokio::time::timeout(std::time::Duration::from_secs(5), session.messaged.notified())
            .await
            .expect("the withdrawn move is written to the session");
        assert_eq!(
            session.messages.lock().unwrap()[0]["details"]["reason"],
            json!("withdrawn")
        );
        assert!(wire.events.lock().unwrap().is_empty());
    }
}
