//! Port of senpi packages/coding-agent/src/modes/interactive/session-control-attach.ts (v2026.10.27-2).
//!
//! What lets a terminal control endpoint serve its session the way a host does: the host's record
//! fanout fed by the one session record projection (`rpc_session_event_record`), so an attached
//! client reads the same records, ordering and in-flight replay as from a host, and a status stream
//! over this one session for `subscribe_sessions` (with the same `mirror` opt-in). Every connection's
//! answers go through the fanout, so a command's answer is ordered with the records around it.
//!
//! The terminal itself is one attachment; each attached client adds one. A client that only subscribed
//! (mirror included) is not an attachment.
//!
//! It also keeps what a host keeps per connection and per session for the drive set (senpi#4267): the
//! roles each connection declared with `set_client_info`, and the admission receipt of every input sent
//! with a `clientMessageId`, so a retry is answered from its receipt instead of being delivered twice.
//!
//! senpi registers a permanent never-attached connection to keep its writer off the stdio lane; the
//! fanout here hands back the records no connection took and they are dropped, which is the same
//! nothing-queues-for-a-sink-that-writes-nowhere outcome.
use crate::session_control_server::ControlConnection;
use maho_core::agent_session::{AgentSession, AgentSessionSubscription};
use maho_rpc::connection_roles::ConnectionRoles;
use maho_rpc::session_event_fanout::SessionEventFanout;
use maho_rpc::session_registry::RpcSessionState;
use maho_rpc::session_status::{SessionStatusEntryView, SessionView};
use maho_rpc::session_status_stream::{
    SessionStatusDelivery, SessionStatusSource, SessionStatusStream,
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex, Weak};

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The receipt a terminal answers an input sent with a `clientMessageId` with, and answers again on a
/// retry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalInputReceipt {
    /// What was sent: a retry with the same id and different content is a conflict, as on a host.
    pub fingerprint: String,
    /// `started` or `queued`.
    pub disposition: &'static str,
    /// `{clientMessageId, durableSessionId, disposition}`.
    pub admission: Value,
}

pub struct ControlAttachments {
    pub fanout: Arc<Mutex<SessionEventFanout>>,
    /// This terminal's one-session status stream (senpi `writer.status`), with the source it reads and
    /// the delivery it writes through.
    pub status: Mutex<SessionStatusStream>,
    pub status_source: TerminalStatusSource,
    pub status_delivery: TerminalStatusDelivery,
    pub roles: Mutex<ConnectionRoles>,
    pub receipts: Mutex<BTreeMap<String, TerminalInputReceipt>>,
    attached: Arc<Mutex<BTreeSet<String>>>,
    records: Mutex<Option<AgentSessionSubscription>>,
    session: Arc<AgentSession>,
}

/// The status rows of this terminal's one session (senpi `writer.status.setSource`).
pub struct TerminalStatusSource {
    session: Weak<AgentSession>,
    attached: Arc<Mutex<BTreeSet<String>>>,
    pending_questions: Arc<dyn Fn() -> usize + Send + Sync>,
}

impl TerminalStatusSource {
    fn entry_view(&self, session: &AgentSession) -> SessionStatusEntryView {
        let attachments = 1 + lock(&self.attached).len();
        SessionStatusEntryView {
            state: RpcSessionState::Open,
            kind: "interactive",
            cwd: session.with_session_manager(|manager| manager.cwd().to_owned()),
            attachments: i64::try_from(attachments).unwrap_or(i64::MAX),
            durable_session_id: Some(session.session_id()),
            session_path: session.session_file(),
            name: session.session_name(),
            context: serde_json::Map::new(),
            open_context: None,
        }
    }
}

impl SessionStatusSource for TerminalStatusSource {
    fn session_ids(&self) -> Vec<String> {
        self.session
            .upgrade()
            .map(|session| vec![session.session_id()])
            .unwrap_or_default()
    }

    fn view(&self, id: &str) -> Option<SessionView> {
        let session = self.session.upgrade()?;
        if id != session.session_id() {
            return None;
        }
        let mut runtime = maho_rpc::session_status_source::session_runtime_view(&session);
        // senpi `sessionRuntimeView(session, pendingQuestions())`: the terminal's own open questions.
        runtime.pending_questions = (self.pending_questions)();
        Some(SessionView {
            entry: self.entry_view(&session),
            runtime: Some(runtime),
        })
    }
}

/// The stream's delivery: the fanout's control lane of the subscribing connection.
pub struct TerminalStatusDelivery {
    fanout: Arc<Mutex<SessionEventFanout>>,
}

impl SessionStatusDelivery for TerminalStatusDelivery {
    fn enqueue(&self, connection_id: &str, line: &str) -> bool {
        lock(&self.fanout).enqueue_line(connection_id, line)
    }

    /// The terminal's questions are counted by the source's runtime view, not by fanout frames.
    fn pending_questions(&self, _session_id: &str) -> usize {
        0
    }
}

/// Which records reach only the connection that caused them (multi-session host `route_session_record`).
fn is_targeted(record: &Value) -> bool {
    matches!(
        record["type"].as_str(),
        Some("response" | "bash_execution_update")
    ) || (record["type"] == "extension_ui_request"
        && matches!(
            record["method"].as_str(),
            Some("select" | "confirm" | "input" | "editor")
        ))
}

fn deliver(
    fanout: &Mutex<SessionEventFanout>,
    session_id: &str,
    target: Option<&str>,
    record: Value,
) {
    let targeted = is_targeted(&record);
    // Records no attached connection takes come back for a stdio lane this endpoint does not have.
    drop(lock(fanout).deliver_owned(session_id, target, targeted, record));
}

impl ControlAttachments {
    pub fn new(
        session: Arc<AgentSession>,
        pending_questions: Arc<dyn Fn() -> usize + Send + Sync>,
    ) -> Self {
        let fanout = Arc::new(Mutex::new(SessionEventFanout::default()));
        let attached = Arc::new(Mutex::new(BTreeSet::new()));
        let status_source = TerminalStatusSource {
            session: Arc::downgrade(&session),
            attached: attached.clone(),
            pending_questions,
        };
        let status_delivery = TerminalStatusDelivery {
            fanout: fanout.clone(),
        };
        let record_fanout = fanout.clone();
        let projected = Arc::downgrade(&session);
        let last_abort_source = maho_rpc::connection_handler::LastAbortSource::default();
        let records = session.subscribe(Arc::new(move |event| {
            let Some(session) = projected.upgrade() else {
                return;
            };
            // The routing handle (this terminal's session id) keys the session's admission ledger.
            let route = session.session_id();
            let Ok(record) = maho_rpc::connection_handler::rpc_session_event_record_routed(
                &session,
                event,
                &last_abort_source,
                Some(&route),
            ) else {
                return;
            };
            deliver(&record_fanout, &session.session_id(), None, record);
        }));
        Self {
            fanout,
            status: Mutex::new(SessionStatusStream::default()),
            status_source,
            status_delivery,
            roles: Mutex::new(ConnectionRoles::default()),
            receipts: Mutex::new(BTreeMap::new()),
            attached,
            records: Mutex::new(Some(records)),
            session,
        }
    }

    /// A client attached or detached (senpi `writer.status.publish(handle, cause, "client")`).
    fn publish(&self, cause: &'static str) {
        lock(&self.status).publish(
            &self.status_source,
            &self.status_delivery,
            &self.handle(),
            cause,
            "client",
            false,
        );
    }

    /// The routing handle records carry: this terminal's session id.
    pub fn handle(&self) -> String {
        self.session.session_id()
    }

    /// The connection's socket writer moves into the fanout: from here on every record it receives,
    /// answers included, leaves through one FIFO.
    pub fn opened(&self, connection: &ControlConnection) {
        if let Some(actor) = lock(&connection.transport).take() {
            lock(&self.fanout).register(&key_of(connection), actor);
        }
    }

    pub fn closed(&self, connection: &ControlConnection) {
        let key = key_of(connection);
        if lock(&self.attached).remove(&key) {
            self.publish("detach");
        }
        lock(&self.roles).release(&key);
        lock(&self.status).drop_connection(&key);
        lock(&self.fanout).unregister(&key);
    }

    /// Drops this connection's attachment (`release_attachment`); false when it was not attached.
    pub fn detach(&self, connection: &ControlConnection) -> bool {
        let key = key_of(connection);
        if !lock(&self.attached).remove(&key) {
            return false;
        }
        lock(&self.fanout).detach(&key, &self.handle());
        self.publish("detach");
        true
    }

    /// Attaches the connection: it is replayed the in-flight snapshot, then receives the session's
    /// records.
    pub fn attach(&self, connection: &ControlConnection) -> bool {
        let key = key_of(connection);
        if !lock(&self.attached).insert(key.clone()) {
            return false;
        }
        drop(lock(&self.fanout).attach(
            &key,
            &self.handle(),
            maho_rpc::connection_handler::now_ms(),
        ));
        self.publish("attach");
        true
    }

    pub fn is_attached(&self, connection: &ControlConnection) -> bool {
        lock(&self.attached).contains(&key_of(connection))
    }

    /// Enqueues a session record with this connection as the current one (its targeted records reach
    /// it), senpi `run(connection, () => writer.enqueue(handle, record))`.
    pub fn enqueue_for(&self, connection: &ControlConnection, record: Value) {
        deliver(
            &self.fanout,
            &self.handle(),
            Some(&key_of(connection)),
            record,
        );
    }

    pub fn respond(&self, connection: &ControlConnection, answer: &Value) -> bool {
        match lock(&self.fanout).enqueue_control(&key_of(connection), answer) {
            Ok(true) => true,
            Ok(false) => false,
            Err(_) => {
                connection.close();
                true
            }
        }
    }

    pub fn has_connection(&self, connection: &ControlConnection) -> bool {
        lock(&self.fanout).is_registered(&key_of(connection))
    }

    pub fn dispose(&self) {
        drop(lock(&self.records).take());
    }
}

/// The fanout's id for one control connection.
pub fn key_of(connection: &ControlConnection) -> String {
    format!("tui-{}", connection.id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answers_and_their_streamed_records_reach_only_the_requesting_connection() {
        assert!(is_targeted(&json!({"type": "response"})));
        assert!(is_targeted(
            &json!({"type": "extension_ui_request", "method": "confirm"})
        ));
        assert!(!is_targeted(
            &json!({"type": "extension_ui_request", "method": "notify"})
        ));
        assert!(!is_targeted(&json!({"type": "agent_end"})));
    }
}
