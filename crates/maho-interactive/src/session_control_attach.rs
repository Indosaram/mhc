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
use maho_rpc::session_status_stream::{SessionStatusSource, SessionStatusStream};
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
    pub status: Arc<SessionStatusStream>,
    pub roles: Mutex<ConnectionRoles>,
    pub receipts: Mutex<BTreeMap<String, TerminalInputReceipt>>,
    attached: Arc<Mutex<BTreeSet<String>>>,
    records: Mutex<Option<AgentSessionSubscription>>,
    session: Arc<AgentSession>,
}

/// The status rows of this terminal's one session (senpi `writer.status.setSource`).
struct TerminalStatusSource {
    session: Weak<AgentSession>,
    attached: Arc<Mutex<BTreeSet<String>>>,
    pending_questions: Arc<dyn Fn() -> usize + Send + Sync>,
}

impl TerminalStatusSource {
    fn session_entry_view(&self, session: &AgentSession) -> Value {
        let (cwd, name) = session.with_session_manager(|manager| {
            (
                manager.cwd().to_owned(),
                manager.session_name().map(str::to_owned),
            )
        });
        let mut view = json!({
            "state": "open",
            "kind": "interactive",
            "cwd": cwd,
            "attachments": 1 + lock(&self.attached).len(),
            "durableSessionId": session.session_id(),
            "context": {},
        });
        if let Some(path) = session.session_file() {
            view["sessionPath"] = path.into();
        }
        if let Some(name) = name {
            view["name"] = name.into();
        }
        view
    }
}

impl SessionStatusSource for TerminalStatusSource {
    fn session_ids(&self) -> Vec<String> {
        self.session
            .upgrade()
            .map(|session| vec![session.session_id()])
            .unwrap_or_default()
    }

    fn entry_view(&self, id: &str) -> Option<Value> {
        let session = self.session.upgrade()?;
        (id == session.session_id()).then(|| self.session_entry_view(&session))
    }

    fn view(&self, id: &str) -> Option<Value> {
        let session = self.session.upgrade()?;
        (id == session.session_id()).then(|| {
            json!({
                "entry": self.session_entry_view(&session),
                "runtime": maho_rpc::session_status_source::session_runtime_view(
                    &session,
                    Some((self.pending_questions)()),
                ),
            })
        })
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
        let status_fanout = fanout.clone();
        let status_questions = pending_questions.clone();
        let status = Arc::new(SessionStatusStream::new(
            maho_rpc::session_status_stream::SessionStatusDelivery {
                // The fanout's control lane serializes the record itself.
                enqueue: Arc::new(move |connection_id: &str, line: &str| {
                    serde_json::from_str::<Value>(line).is_ok_and(|record| {
                        lock(&status_fanout)
                            .enqueue_control(connection_id, &record)
                            .unwrap_or(false)
                    })
                }),
                pending_questions: Arc::new(move |_session_id: &str| status_questions()),
                now: None,
            },
        ));
        status.set_source(Arc::new(TerminalStatusSource {
            session: Arc::downgrade(&session),
            attached: attached.clone(),
            pending_questions,
        }));
        let record_fanout = fanout.clone();
        let projected = Arc::downgrade(&session);
        let last_abort_source = maho_rpc::connection_handler::LastAbortSource::default();
        let records = session.subscribe(Arc::new(move |event| {
            let Some(session) = projected.upgrade() else {
                return;
            };
            let Ok(record) = maho_rpc::connection_handler::rpc_session_event_record(
                &session,
                event,
                &last_abort_source,
            ) else {
                return;
            };
            deliver(&record_fanout, &session.session_id(), None, record);
        }));
        Self {
            fanout,
            status,
            roles: Mutex::new(ConnectionRoles::default()),
            receipts: Mutex::new(BTreeMap::new()),
            attached,
            records: Mutex::new(Some(records)),
            session,
        }
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
            self.status.publish(&self.handle(), "detach", "client");
        }
        lock(&self.roles).release(&key);
        self.status.drop_connection(&key);
        lock(&self.fanout).unregister(&key);
    }

    /// Drops this connection's attachment (`release_attachment`); false when it was not attached.
    pub fn detach(&self, connection: &ControlConnection) -> bool {
        let key = key_of(connection);
        if !lock(&self.attached).remove(&key) {
            return false;
        }
        lock(&self.fanout).detach(&key, &self.handle());
        self.status.publish(&self.handle(), "detach", "client");
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
        self.status.publish(&self.handle(), "attach", "client");
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
