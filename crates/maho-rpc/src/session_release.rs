//! `release_session`: a host hands one of its sessions to a runtime outside it (senpi
//! `modes/rpc/session-release.ts`) - `omo daemon adopt` resumes it in a local terminal with
//! `senpi --session <session_path>`.
//!
//! The hand-over is a teardown, not a transfer: nothing is replayed, the runtime is disposed and the path
//! reservation is released (the in-process one and the cross-generation claim), so the next process to write the
//! file is the one the caller starts. Before that the transcript gains one `session_released` entry - a `custom`
//! bookkeeping entry the model never sees - so the file itself records which host let go of it and when. Clients
//! still attached (only with `force`) receive `session_closed { reason: "released" }`, which tells them NOT to
//! reopen the path here.
//!
//! A session is released only when it is QUIET: no agent run, no prompt still in preflight, no queued user input,
//! no admitted delivery waiting to be written, no bash, compaction or barrier-held session work (the fields the
//! handoff park judges by), and no other request for the session in flight on any connection. Busy is refused
//! with nothing changed - `turn_active` when a turn is running or about to start, `session_busy` for other work,
//! both naming the signals in `errorData.busy` - unless `interrupt` is set: then the run and any bash are
//! aborted, the release waits (bounded) for the run to go idle and for the other requests and prompts to settle,
//! and checks again. The last check and the close claim run in one synchronous step, so a command routed after it
//! finds the session closing, and work started before it is seen by it. Also refused while clients are attached
//! (`attached`, unless `force`) and for a session this host cannot hand over (`release_unsupported`: a worker
//! isolate owns the runtime, or there is no file).
use crate::handoff_activity::is_handoff_busy;
use crate::rpc_types::{
    RPC_ERROR_ATTACHED, RPC_ERROR_HOST_DRAINING, RPC_ERROR_INVALID_RELEASE_REASON,
    RPC_ERROR_RELEASE_FAILED, RPC_ERROR_RELEASE_UNSUPPORTED, RPC_ERROR_SESSION_BUSY,
    RPC_ERROR_SESSION_CLOSING, RPC_ERROR_TURN_ACTIVE,
};
use crate::session_release_interrupt::{
    ReleasableSession, ReleaseDropped, ReleaseSettlePort, ReleaseWriteError, abort_and_settle,
    take_queued_input,
};
use async_trait::async_trait;
use maho_core::session_move::bundle::{is_move_id, is_plain_id_str};
use serde_json::{Map, Value, json};
use std::sync::Arc;

/// `customType` of the transcript entry a release appends.
pub const SESSION_RELEASED_ENTRY_TYPE: &str = "session_released";

/// What `pi.session.admitExternalMessage` throws once a release has closed admission.
pub const RELEASED_ADMISSION_CLOSED: &str = "release_session handed this session to another runtime; deliver there instead (admission is closed here)";

/// `errorData.hint` of a refusal whose `busy` names `queued`: only an interrupt hands that input back.
pub const RELEASE_QUEUED_HINT: &str = "queued user input is still owed a turn; release with interrupt: true to take it out and receive it in dropped.user_messages";

// `rpc-types.ts` wire codes `release_session` answers with.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReleaseBusySignal {
    Turn,
    Prompt,
    Queued,
    Delivery,
    Bash,
    Compaction,
    SessionWork,
    Activity,
    Request,
}

impl ReleaseBusySignal {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Turn => "turn",
            Self::Prompt => "prompt",
            Self::Queued => "queued",
            Self::Delivery => "delivery",
            Self::Bash => "bash",
            Self::Compaction => "compaction",
            Self::SessionWork => "session_work",
            Self::Activity => "activity",
            Self::Request => "request",
        }
    }
}

/// `release_session`'s arguments, as read off the wire command.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ReleaseSessionCommand {
    pub id: Option<String>,
    pub session_id: String,
    /// `takeover` or `move`; anything else is refused. Kept as a JSON value so a non-string is refused too.
    pub reason: Value,
    pub move_id: Value,
    pub target: Option<String>,
    pub force: Option<bool>,
    pub interrupt: Option<bool>,
}

impl ReleaseSessionCommand {
    /// The wire command into its arguments; `None` when the command is not a `release_session`.
    pub fn from_wire(value: &Value) -> Option<Self> {
        let object = value.as_object()?;
        if object.get("type").and_then(Value::as_str) != Some("release_session") {
            return None;
        }
        Some(Self {
            id: object.get("id").and_then(Value::as_str).map(str::to_owned),
            session_id: object
                .get("sessionId")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            reason: object.get("reason").cloned().unwrap_or(Value::Null),
            move_id: object.get("moveId").cloned().unwrap_or(Value::Null),
            target: object
                .get("target")
                .and_then(Value::as_str)
                .map(str::to_owned),
            force: object.get("force").and_then(Value::as_bool),
            interrupt: object.get("interrupt").and_then(Value::as_bool),
        })
    }

    fn reason_str(&self) -> Option<&str> {
        self.reason.as_str()
    }
}

/// What the router lends a live entry: its state and its release-relevant fields.
pub struct ReleaseEntry {
    /// `entry.state === "open"`.
    pub open: bool,
    /// `entry.runtime?.session`; `None` when a worker isolate owns the runtime.
    pub session: Option<Arc<dyn ReleasableSession>>,
    /// `entry.sessionPath ?? session.sessionFile`.
    pub session_path: Option<String>,
    pub attachments: u64,
}

/// A failed lookup: the wire code (`unknown_session`, `session_closing`) the throw carried.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LookupFailure(pub String);

/// What the router lends a release: its lookup, its request accounting, and the park teardown.
#[async_trait]
pub trait SessionReleasePort: ReleaseSettlePort {
    fn draining(&self) -> bool;
    fn host_instance(&self) -> Option<String>;
    /// The live entry, or the wire code of the lookup failure.
    async fn lookup(&self, session_id: &str) -> Result<ReleaseEntry, LookupFailure>;
    /// Requests for the session in flight on any connection, the release itself not counted.
    fn other_requests(&self, session_id: &str) -> usize;
    /// Claims every attachment and tears the session down, sealing it as released. Its claim is taken before its
    /// first await. `false` when another close already owns the entry.
    async fn tear_down(&self, session_id: &str, session_path: &str) -> bool;
}

struct Releasable {
    session: Arc<dyn ReleasableSession>,
    session_path: String,
    attachments: u64,
}

fn refusal(id: Option<&str>, code: &str, data: Option<Map<String, Value>>) -> Value {
    let mut response = Map::new();
    if let Some(id) = id {
        response.insert("id".into(), json!(id));
    }
    response.insert("type".into(), json!("response"));
    response.insert("command".into(), json!("release_session"));
    response.insert("success".into(), json!(false));
    response.insert("error".into(), json!(code));
    if let Some(data) = data {
        response.insert("errorCode".into(), json!(code));
        response.insert("errorData".into(), Value::Object(data));
    }
    Value::Object(response)
}

fn object(pairs: Vec<(&str, Value)>) -> Map<String, Value> {
    pairs
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value))
        .collect()
}

fn taken_data(interrupted: bool, dropped: &ReleaseDropped) -> Option<Map<String, Value>> {
    interrupted.then(|| {
        object(vec![
            ("interrupted", json!(true)),
            ("dropped", dropped.to_value()),
        ])
    })
}

fn merged(
    first: Option<Map<String, Value>>,
    second: Option<&Map<String, Value>>,
) -> Option<Map<String, Value>> {
    match (first, second) {
        (None, None) => None,
        (first, second) => {
            let mut map = first.unwrap_or_default();
            if let Some(second) = second {
                map.extend(second.clone());
            }
            Some(map)
        }
    }
}

fn busy_signals(
    port: &dyn SessionReleasePort,
    session_id: &str,
    session: &dyn ReleasableSession,
) -> Vec<ReleaseBusySignal> {
    let activity = session.activity_snapshot();
    let mut signals = Vec::new();
    if activity.is_streaming {
        signals.push(ReleaseBusySignal::Turn);
    }
    if !port.pending_prompts(session_id).is_empty() {
        signals.push(ReleaseBusySignal::Prompt);
    }
    // Queued user input outlives its run (a steer can end the stream it was aimed at): it is still owed a turn.
    if session.pending_message_count() > 0 {
        signals.push(ReleaseBusySignal::Queued);
    }
    if !session.admission_pending().is_empty() {
        signals.push(ReleaseBusySignal::Delivery);
    }
    if activity.is_bash_running {
        signals.push(ReleaseBusySignal::Bash);
    }
    if activity.is_compacting {
        signals.push(ReleaseBusySignal::Compaction);
    }
    if activity.has_session_work {
        signals.push(ReleaseBusySignal::SessionWork);
    }
    // The handoff park's predicate decides; a source added to it later is not missed here.
    if signals.is_empty() && is_handoff_busy(activity) {
        signals.push(ReleaseBusySignal::Activity);
    }
    if port.other_requests(session_id) > 0 {
        signals.push(ReleaseBusySignal::Request);
    }
    signals
}

fn busy_data(signals: &[ReleaseBusySignal]) -> Map<String, Value> {
    let names: Vec<&str> = signals.iter().map(|signal| signal.as_str()).collect();
    if signals.contains(&ReleaseBusySignal::Queued) {
        object(vec![
            ("busy", json!(names)),
            ("retry_with", json!({"interrupt": true})),
            ("hint", json!(RELEASE_QUEUED_HINT)),
        ])
    } else {
        object(vec![("busy", json!(names))])
    }
}

fn busy_code(signals: &[ReleaseBusySignal]) -> &'static str {
    let turn_owed = signals.iter().any(|signal| {
        matches!(
            signal,
            ReleaseBusySignal::Turn
                | ReleaseBusySignal::Prompt
                | ReleaseBusySignal::Queued
                | ReleaseBusySignal::Delivery
        )
    });
    if turn_owed {
        RPC_ERROR_TURN_ACTIVE
    } else {
        RPC_ERROR_SESSION_BUSY
    }
}

/// `taken`: what an interrupt already took out of the session, merged into any refusal's `errorData`.
async fn releasable(
    port: &dyn SessionReleasePort,
    command: &ReleaseSessionCommand,
    taken: Option<&Map<String, Value>>,
) -> Result<Releasable, Value> {
    let refuse = |code: &str, data: Option<Map<String, Value>>| {
        let data = if data.is_some() || taken.is_some() {
            merged(data, taken)
        } else {
            None
        };
        refusal(command.id.as_deref(), code, data)
    };
    let entry = match port.lookup(&command.session_id).await {
        Ok(entry) => entry,
        Err(LookupFailure(code)) => return Err(refuse(&code, None)),
    };
    if !entry.open {
        return Err(refuse(RPC_ERROR_SESSION_CLOSING, None));
    }
    let Some(session) = entry.session else {
        return Err(refuse(
            RPC_ERROR_RELEASE_UNSUPPORTED,
            Some(object(vec![("detail", json!("worker_runtime"))])),
        ));
    };
    let Some(session_path) = entry.session_path else {
        return Err(refuse(
            RPC_ERROR_RELEASE_UNSUPPORTED,
            Some(object(vec![("detail", json!("no_session_file"))])),
        ));
    };
    if entry.attachments > 0 && command.force != Some(true) {
        return Err(refuse(
            RPC_ERROR_ATTACHED,
            Some(object(vec![("attachments", json!(entry.attachments))])),
        ));
    }
    Ok(Releasable {
        session,
        session_path,
        attachments: entry.attachments,
    })
}

/// Reopens admission on every answer except a successful release.
struct ReopenUnlessReleased<'a> {
    session: &'a dyn ReleasableSession,
    released: bool,
}

impl Drop for ReopenUnlessReleased<'_> {
    fn drop(&mut self) {
        if !self.released {
            self.session.admission_reopen();
        }
    }
}

pub async fn release_session(
    port: &dyn SessionReleasePort,
    command: &ReleaseSessionCommand,
) -> Value {
    let id = command.id.as_deref();
    // Export and the receiver's ledger accept only a plain move id; a release under any other id could never finish.
    let valid_move = command.reason_str() == Some("move") && is_move_id(&command.move_id);
    if command.reason_str() != Some("takeover") && !valid_move {
        return refusal(id, RPC_ERROR_INVALID_RELEASE_REASON, None);
    }
    if port.draining() {
        return refusal(id, RPC_ERROR_HOST_DRAINING, None);
    }
    let first = match releasable(port, command, None).await {
        Ok(first) => first,
        Err(answer) => return answer,
    };
    // The seal records the session's own id, which the bundle and the receiver use as a folder name: a move of a
    // session whose id is not a plain id is refused here, before anything is interrupted or sealed (never
    // sanitized).
    if command.reason_str() == Some("move") && !is_plain_id_str(&first.session.session_id()) {
        return refusal(
            id,
            RPC_ERROR_RELEASE_UNSUPPORTED,
            Some(object(vec![("detail", json!("session_id_not_plain"))])),
        );
    }
    let busy = busy_signals(port, &command.session_id, first.session.as_ref());
    let interrupted = !busy.is_empty();
    if interrupted && command.interrupt != Some(true) {
        let mut data = object(vec![("attachments", json!(first.attachments))]);
        data.extend(busy_data(&busy));
        return refusal(id, busy_code(&busy), Some(data));
    }
    // Admission closes below (for the interrupt, or at the claim). Every answer except a release reopens it on
    // the session the host keeps, and a failure is answered `release_failed` rather than left unanswered.
    let mut guard = ReopenUnlessReleased {
        session: first.session.as_ref(),
        released: false,
    };
    let mut dropped = ReleaseDropped::nothing();
    let attempt: Result<Value, ReleaseWriteError> = async {
        if interrupted {
            first.session.admission_close(RELEASED_ADMISSION_CLOSED);
            dropped = take_queued_input(first.session.as_ref());
            abort_and_settle(port, &command.session_id, &first.session).await;
            // What the clear retired is on disk before `session_released` seals the file (senpi#3549).
            first.session.when_queue_clear_recorded().await;
        }
        // A never-written session gets its file here, before the final check: its `session_released` entry must
        // reach disk, and a header write that fails is this release's failure, never a stray rejection. A
        // written session awaits nothing, so its claim stays in the same turn as the request that asked.
        if !first.session.is_transcript_flushed() {
            first
                .session
                .persist_header_now()
                .await
                .map_err(ReleaseWriteError::Other)?;
        }
        claim_and_release(port, command, interrupted, &dropped).await
    }
    .await;
    let taken = taken_data(interrupted, &dropped);
    let answer = match attempt {
        Ok(answer) => answer,
        Err(ReleaseWriteError::MovedAway(error)) => {
            let mut data = object(vec![("detail", json!(error.to_string()))]);
            data.extend(taken.unwrap_or_default());
            refusal(id, error.code(), Some(data))
        }
        Err(ReleaseWriteError::Other(detail)) => {
            let mut data = object(vec![("detail", json!(detail))]);
            data.extend(taken.unwrap_or_default());
            refusal(id, RPC_ERROR_RELEASE_FAILED, Some(data))
        }
    };
    guard.released = answer.get("success").and_then(Value::as_bool) == Some(true);
    answer
}

/// The final check and the release. After an interrupt every refusal carries `interrupted` and `dropped`: the
/// queues were already emptied, so what they held must reach the caller whatever the answer.
async fn claim_and_release(
    port: &dyn SessionReleasePort,
    command: &ReleaseSessionCommand,
    interrupted: bool,
    dropped: &ReleaseDropped,
) -> Result<Value, ReleaseWriteError> {
    let id = command.id.as_deref();
    let taken = taken_data(interrupted, dropped);
    let ready = match releasable(port, command, taken.as_ref()).await {
        Ok(ready) => ready,
        Err(answer) => return Ok(answer),
    };
    let still_busy = busy_signals(port, &command.session_id, ready.session.as_ref());
    if !still_busy.is_empty() {
        let mut data = object(vec![("attachments", json!(ready.attachments))]);
        data.extend(busy_data(&still_busy));
        data.insert("interrupted".into(), json!(interrupted));
        data.extend(taken.clone().unwrap_or_default());
        return Ok(refusal(id, busy_code(&still_busy), Some(data)));
    }
    // From here to the close claim nothing awaits: a drain pass still running admits nothing more, so a delivery
    // it had not admitted stays with its sender and reaches the next owner.
    ready.session.admission_close(RELEASED_ADMISSION_CLOSED);
    let mut entry = object(vec![("reason", command.reason.clone())]);
    if command.reason_str() == Some("move") {
        entry.insert("move_id".into(), command.move_id.clone());
        entry.insert(
            "target".into(),
            json!(command.target.clone().unwrap_or_default()),
        );
        entry.insert("session_id".into(), json!(ready.session.session_id()));
    }
    entry.insert("interrupted".into(), json!(interrupted));
    entry.insert("attachments".into(), json!(ready.attachments));
    entry.insert("host_instance".into(), json!(port.host_instance()));
    entry.insert(
        "released_at".into(),
        json!(chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)),
    );
    // On disk or not at all: a failed write leaves no entry a later append could chain onto.
    if let Err(cause) = ready
        .session
        .append_custom_entry(SESSION_RELEASED_ENTRY_TYPE, Value::Object(entry))
    {
        ready.session.admission_reopen();
        return Err(cause);
    }
    if !port
        .tear_down(&command.session_id, &ready.session_path)
        .await
    {
        return Ok(refusal(id, RPC_ERROR_SESSION_CLOSING, taken));
    }
    let mut response = Map::new();
    if let Some(id) = id {
        response.insert("id".into(), json!(id));
    }
    response.insert("type".into(), json!("response"));
    response.insert("command".into(), json!("release_session"));
    response.insert("success".into(), json!(true));
    response.insert(
        "data".into(),
        json!({
            "released": true,
            "session_path": ready.session_path,
            "attachments": ready.attachments,
            "dropped": dropped.to_value(),
        }),
    );
    Ok(Value::Object(response))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_release_interrupt::{BoxFuture, SubscriptionGuard};
    use maho_core::agent_session::ClearedQueue;
    use maho_core::session_activity::SessionActivitySnapshot;
    use maho_core::session_move::ownership::{
        RefusedOwnership, RefusedState, SessionMovedAwayError,
    };
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    #[derive(Default)]
    struct Fake {
        activity: Mutex<SessionActivitySnapshot>,
        queued: Mutex<Vec<String>>,
        pending: Mutex<Vec<String>>,
        flushed: AtomicBool,
        id: Mutex<String>,
        closed: AtomicUsize,
        reopened: AtomicUsize,
        aborts: AtomicUsize,
        appended: Mutex<Vec<(String, Value)>>,
        append_error: Mutex<Option<ReleaseWriteError>>,
        header_error: Mutex<Option<String>>,
    }

    #[derive(Clone)]
    struct Handle(Arc<Fake>);

    #[async_trait]
    impl ReleasableSession for Handle {
        fn activity_snapshot(&self) -> SessionActivitySnapshot {
            *self.0.activity.lock().unwrap()
        }
        fn pending_message_count(&self) -> usize {
            self.0.queued.lock().unwrap().len()
        }
        fn admission_pending(&self) -> Vec<String> {
            self.0.pending.lock().unwrap().clone()
        }
        fn admission_close(&self, _: &str) {
            self.0.closed.fetch_add(1, Ordering::SeqCst);
        }
        fn admission_reopen(&self) {
            self.0.reopened.fetch_add(1, Ordering::SeqCst);
        }
        fn clear_queue(&self, _: bool) -> ClearedQueue {
            let queued = std::mem::take(&mut *self.0.queued.lock().unwrap());
            self.0.pending.lock().unwrap().clear();
            ClearedQueue {
                steering: queued.clone(),
                follow_up: Vec::new(),
                ordered: queued
                    .into_iter()
                    .enumerate()
                    .map(|(order, text)| maho_core::agent_session::QueuedInput {
                        text,
                        mode: maho_ext_api::StreamingBehavior::Steer,
                        enqueue_order: order as u64,
                    })
                    .collect(),
            }
        }
        fn abort_bash(&self) {}
        fn on_agent_start(&self, _: Arc<dyn Fn() + Send + Sync>) -> SubscriptionGuard {
            Box::new(())
        }
        async fn abort(&self) {
            self.0.aborts.fetch_add(1, Ordering::SeqCst);
            *self.0.activity.lock().unwrap() = SessionActivitySnapshot::default();
        }
        async fn wait_for_idle(&self) {}
        async fn when_queue_clear_recorded(&self) {}
        fn session_id(&self) -> String {
            self.0.id.lock().unwrap().clone()
        }
        fn is_transcript_flushed(&self) -> bool {
            self.0.flushed.load(Ordering::SeqCst)
        }
        async fn persist_header_now(&self) -> Result<(), String> {
            match self.0.header_error.lock().unwrap().clone() {
                Some(error) => Err(error),
                None => Ok(()),
            }
        }
        fn append_custom_entry(
            &self,
            custom_type: &str,
            data: Value,
        ) -> Result<(), ReleaseWriteError> {
            if let Some(error) = self.0.append_error.lock().unwrap().clone() {
                return Err(error);
            }
            self.0
                .appended
                .lock()
                .unwrap()
                .push((custom_type.to_owned(), data));
            Ok(())
        }
    }

    struct Port {
        fake: Arc<Fake>,
        draining: bool,
        attachments: u64,
        open: bool,
        worker: bool,
        path: Option<String>,
        others: AtomicUsize,
        torn_down: AtomicUsize,
        tear_down_wins: bool,
    }

    fn port(fake: &Arc<Fake>) -> Port {
        Port {
            fake: Arc::clone(fake),
            draining: false,
            attachments: 0,
            open: true,
            worker: false,
            path: Some("/s.jsonl".into()),
            others: AtomicUsize::new(0),
            torn_down: AtomicUsize::new(0),
            tear_down_wins: true,
        }
    }

    #[async_trait]
    impl ReleaseSettlePort for Port {
        async fn other_requests_settled(&self, _: &str) {}
        fn pending_prompts(&self, _: &str) -> Vec<BoxFuture<'static, ()>> {
            Vec::new()
        }
    }

    #[async_trait]
    impl SessionReleasePort for Port {
        fn draining(&self) -> bool {
            self.draining
        }
        fn host_instance(&self) -> Option<String> {
            Some("host-1".into())
        }
        async fn lookup(&self, session_id: &str) -> Result<ReleaseEntry, LookupFailure> {
            if session_id != "h1" {
                return Err(LookupFailure("unknown_session".into()));
            }
            Ok(ReleaseEntry {
                open: self.open,
                session: (!self.worker).then(|| {
                    Arc::new(Handle(Arc::clone(&self.fake))) as Arc<dyn ReleasableSession>
                }),
                session_path: self.path.clone(),
                attachments: self.attachments,
            })
        }
        fn other_requests(&self, _: &str) -> usize {
            self.others.load(Ordering::SeqCst)
        }
        async fn tear_down(&self, _: &str, _: &str) -> bool {
            self.torn_down.fetch_add(1, Ordering::SeqCst);
            self.tear_down_wins
        }
    }

    fn fake() -> Arc<Fake> {
        let fake = Arc::new(Fake::default());
        fake.flushed.store(true, Ordering::SeqCst);
        *fake.id.lock().unwrap() = "sess-1".into();
        fake
    }

    fn takeover() -> ReleaseSessionCommand {
        ReleaseSessionCommand {
            id: Some("r1".into()),
            session_id: "h1".into(),
            reason: json!("takeover"),
            ..Default::default()
        }
    }

    fn mover(move_id: Value) -> ReleaseSessionCommand {
        ReleaseSessionCommand {
            reason: json!("move"),
            move_id,
            target: Some("laptop".into()),
            ..takeover()
        }
    }

    fn code(answer: &Value) -> &str {
        answer["error"].as_str().unwrap_or_default()
    }

    #[tokio::test]
    async fn releases_a_quiet_session_and_records_the_takeover_entry() {
        let fake = fake();
        let port = port(&fake);
        let answer = release_session(&port, &takeover()).await;
        assert_eq!(answer["success"], json!(true));
        assert_eq!(
            answer["data"],
            json!({"released":true,"session_path":"/s.jsonl","attachments":0,"dropped":{"deliveries":[],"user_messages":[]}})
        );
        assert_eq!(port.torn_down.load(Ordering::SeqCst), 1);
        let appended = fake.appended.lock().unwrap();
        assert_eq!(appended.len(), 1);
        assert_eq!(appended[0].0, SESSION_RELEASED_ENTRY_TYPE);
        assert_eq!(appended[0].1["reason"], json!("takeover"));
        assert_eq!(appended[0].1["host_instance"], json!("host-1"));
        assert!(appended[0].1.get("move_id").is_none());
        assert_eq!(fake.closed.load(Ordering::SeqCst), 1);
        // A release leaves admission closed.
        assert_eq!(fake.reopened.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn a_move_seals_with_its_move_id_target_and_session_id() {
        let fake = fake();
        let answer = release_session(&port(&fake), &mover(json!("m-1"))).await;
        assert_eq!(answer["success"], json!(true));
        let appended = fake.appended.lock().unwrap();
        assert_eq!(appended[0].1["move_id"], json!("m-1"));
        assert_eq!(appended[0].1["target"], json!("laptop"));
        assert_eq!(appended[0].1["session_id"], json!("sess-1"));
    }

    #[tokio::test]
    async fn refuses_unknown_reasons_and_unplain_move_ids() {
        let fake = fake();
        let port = port(&fake);
        for command in [
            ReleaseSessionCommand {
                reason: json!("other"),
                ..takeover()
            },
            ReleaseSessionCommand {
                reason: json!(7),
                ..takeover()
            },
            mover(Value::Null),
            mover(json!("../escape")),
            mover(json!("a".repeat(129))),
        ] {
            assert_eq!(
                code(&release_session(&port, &command).await),
                RPC_ERROR_INVALID_RELEASE_REASON
            );
        }
        assert_eq!(fake.closed.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn refuses_a_move_of_a_session_whose_id_is_not_plain() {
        let fake = fake();
        *fake.id.lock().unwrap() = "../up".into();
        let answer = release_session(&port(&fake), &mover(json!("m-1"))).await;
        assert_eq!(code(&answer), RPC_ERROR_RELEASE_UNSUPPORTED);
        assert_eq!(answer["errorData"]["detail"], json!("session_id_not_plain"));
        assert!(fake.appended.lock().unwrap().is_empty());
        // A takeover does not record the id, so it is not subject to this.
        assert_eq!(
            release_session(&port(&fake), &takeover()).await["success"],
            json!(true)
        );
    }

    #[tokio::test]
    async fn refuses_while_draining_and_for_unsupported_or_closing_entries() {
        let fake = fake();
        let mut draining = port(&fake);
        draining.draining = true;
        assert_eq!(
            code(&release_session(&draining, &takeover()).await),
            RPC_ERROR_HOST_DRAINING
        );

        let mut closing = port(&fake);
        closing.open = false;
        assert_eq!(
            code(&release_session(&closing, &takeover()).await),
            RPC_ERROR_SESSION_CLOSING
        );

        let mut worker = port(&fake);
        worker.worker = true;
        let answer = release_session(&worker, &takeover()).await;
        assert_eq!(code(&answer), RPC_ERROR_RELEASE_UNSUPPORTED);
        assert_eq!(answer["errorData"]["detail"], json!("worker_runtime"));

        let mut pathless = port(&fake);
        pathless.path = None;
        let answer = release_session(&pathless, &takeover()).await;
        assert_eq!(answer["errorData"]["detail"], json!("no_session_file"));

        let unknown = ReleaseSessionCommand {
            session_id: "nope".into(),
            ..takeover()
        };
        assert_eq!(
            code(&release_session(&port(&fake), &unknown).await),
            "unknown_session"
        );
    }

    #[tokio::test]
    async fn refuses_attached_clients_unless_forced() {
        let fake = fake();
        let mut attached = port(&fake);
        attached.attachments = 2;
        let answer = release_session(&attached, &takeover()).await;
        assert_eq!(code(&answer), RPC_ERROR_ATTACHED);
        assert_eq!(answer["errorData"]["attachments"], json!(2));
        let forced = ReleaseSessionCommand {
            force: Some(true),
            ..takeover()
        };
        let answer = release_session(&attached, &forced).await;
        assert_eq!(answer["success"], json!(true));
        assert_eq!(answer["data"]["attachments"], json!(2));
    }

    #[tokio::test]
    async fn a_busy_session_is_refused_with_its_signals_and_nothing_changed() {
        let fake = fake();
        fake.activity.lock().unwrap().is_streaming = true;
        let answer = release_session(&port(&fake), &takeover()).await;
        assert_eq!(code(&answer), RPC_ERROR_TURN_ACTIVE);
        assert_eq!(answer["errorData"]["busy"], json!(["turn"]));
        assert_eq!(fake.closed.load(Ordering::SeqCst), 0);
        assert_eq!(fake.aborts.load(Ordering::SeqCst), 0);

        let fake = self::fake();
        fake.activity.lock().unwrap().is_bash_running = true;
        let answer = release_session(&port(&fake), &takeover()).await;
        assert_eq!(code(&answer), RPC_ERROR_SESSION_BUSY);
        assert_eq!(answer["errorData"]["busy"], json!(["bash"]));

        let fake = self::fake();
        let port = port(&fake);
        port.others.store(1, Ordering::SeqCst);
        assert_eq!(
            release_session(&port, &takeover()).await["errorData"]["busy"],
            json!(["request"])
        );
    }

    #[tokio::test]
    async fn queued_input_names_the_interrupt_retry() {
        let fake = fake();
        *fake.queued.lock().unwrap() = vec!["owed".into()];
        let answer = release_session(&port(&fake), &takeover()).await;
        assert_eq!(code(&answer), RPC_ERROR_TURN_ACTIVE);
        assert_eq!(
            answer["errorData"]["retry_with"],
            json!({"interrupt": true})
        );
        assert_eq!(answer["errorData"]["hint"], json!(RELEASE_QUEUED_HINT));
    }

    #[tokio::test]
    async fn an_interrupt_takes_the_queue_aborts_and_releases() {
        let fake = fake();
        fake.activity.lock().unwrap().is_streaming = true;
        *fake.queued.lock().unwrap() = vec!["owed".into()];
        *fake.pending.lock().unwrap() = vec!["d1".into()];
        let command = ReleaseSessionCommand {
            interrupt: Some(true),
            ..takeover()
        };
        let answer = release_session(&port(&fake), &command).await;
        assert_eq!(answer["success"], json!(true));
        assert_eq!(
            answer["data"]["dropped"],
            json!({"deliveries":["d1"],"user_messages":["owed"]})
        );
        assert!(fake.aborts.load(Ordering::SeqCst) >= 1);
        assert_eq!(
            fake.appended.lock().unwrap()[0].1["interrupted"],
            json!(true)
        );
    }

    #[tokio::test]
    async fn a_failed_write_reopens_admission_and_answers_release_failed_with_what_was_taken() {
        let fake = fake();
        fake.activity.lock().unwrap().is_streaming = true;
        *fake.queued.lock().unwrap() = vec!["owed".into()];
        *fake.append_error.lock().unwrap() = Some(ReleaseWriteError::Other("disk full".into()));
        let command = ReleaseSessionCommand {
            interrupt: Some(true),
            ..takeover()
        };
        let answer = release_session(&port(&fake), &command).await;
        assert_eq!(code(&answer), RPC_ERROR_RELEASE_FAILED);
        assert_eq!(answer["errorData"]["detail"], json!("disk full"));
        assert_eq!(answer["errorData"]["interrupted"], json!(true));
        assert_eq!(
            answer["errorData"]["dropped"]["user_messages"],
            json!(["owed"])
        );
        assert!(fake.reopened.load(Ordering::SeqCst) >= 1);
    }

    #[tokio::test]
    async fn a_header_write_failure_is_this_releases_failure() {
        let fake = fake();
        fake.flushed.store(false, Ordering::SeqCst);
        *fake.header_error.lock().unwrap() = Some("EACCES".into());
        let answer = release_session(&port(&fake), &takeover()).await;
        assert_eq!(code(&answer), RPC_ERROR_RELEASE_FAILED);
        assert_eq!(answer["errorData"]["detail"], json!("EACCES"));
        assert!(fake.appended.lock().unwrap().is_empty());
        assert_eq!(fake.reopened.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_moved_away_file_is_refused_with_its_own_code() {
        let fake = fake();
        let moved = SessionMovedAwayError::new(
            "/s.jsonl",
            RefusedOwnership {
                state: RefusedState::Moved,
                move_id: "m".into(),
                target: "t".into(),
            },
        );
        *fake.append_error.lock().unwrap() = Some(ReleaseWriteError::MovedAway(moved.clone()));
        let answer = release_session(&port(&fake), &takeover()).await;
        assert_eq!(code(&answer), "session_moved_away");
        assert_eq!(answer["errorData"]["detail"], json!(moved.to_string()));
    }

    #[tokio::test]
    async fn losing_the_close_claim_answers_session_closing_and_reopens_admission() {
        let fake = fake();
        let mut port = port(&fake);
        port.tear_down_wins = false;
        let answer = release_session(&port, &takeover()).await;
        assert_eq!(code(&answer), RPC_ERROR_SESSION_CLOSING);
        assert_eq!(fake.reopened.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn parses_the_wire_command() {
        let command = ReleaseSessionCommand::from_wire(&json!({
            "type":"release_session","id":"1","sessionId":"h1","reason":"move","moveId":"m","target":"t","force":true,"interrupt":false
        }))
        .unwrap();
        assert_eq!(command.session_id, "h1");
        assert_eq!(command.force, Some(true));
        assert_eq!(command.interrupt, Some(false));
        assert!(ReleaseSessionCommand::from_wire(&json!({"type":"prompt"})).is_none());
    }
}
