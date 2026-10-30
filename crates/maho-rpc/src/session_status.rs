//! The `session_status` record and the ONE pure function that derives it (senpi `rpc/session-status.ts`).
//!
//! Every status a subscriber sees is computed by [`derive_session_status`] from three inputs the host
//! already holds: the registry entry, the session's own runtime view and the fanout's pending question
//! frames. Nothing here reads a clock, a socket or a registry.
use crate::session_registry::RpcSessionState;
use maho_ext_api::SessionKind;
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatusValue {
    Opening,
    Idle,
    Running,
    WaitingInput,
    Compacting,
    Unloaded,
    Closed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatusCause {
    Open,
    Prompt,
    AgentEnd,
    AgentIdle,
    Abort,
    Question,
    Compaction,
    SetModel,
    Queue,
    Attach,
    Detach,
    Replace,
    Park,
    Close,
    Context,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatusActor {
    Client,
    Agent,
    Host,
}

/// A session's published labels: the open-time context overlaid with every `set_session_context` patch.
/// Values are JSON strings, numbers or booleans.
pub type PublishedSessionContext = BTreeMap<String, Value>;

/// Why a session's routing handle ended; set once, from the record that sealed it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionStatusTerminal {
    /// `Unloaded` or `Closed`.
    pub status: SessionStatusValue,
    pub reason: Option<String>,
}

/// What the registry entry contributes.
#[derive(Clone, Debug)]
pub struct SessionStatusEntryView {
    pub state: RpcSessionState,
    pub kind: SessionKind,
    pub cwd: String,
    pub attachments: i64,
    pub durable_session_id: Option<String>,
    pub session_path: Option<String>,
    pub name: Option<String>,
    pub context: PublishedSessionContext,
    /// The context the session was opened with, before any patch; `childrenOf` matching reads this.
    pub open_context: Option<PublishedSessionContext>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SessionStatusModel {
    pub provider: String,
    pub id: String,
}

/// What the live runtime contributes; absent while the session is opening or after teardown.
#[derive(Clone, Debug, Default)]
pub struct SessionStatusRuntimeView {
    pub is_streaming: bool,
    pub is_compacting: bool,
    pub model: Option<SessionStatusModel>,
    pub thinking_level: Option<String>,
    pub steering: u64,
    pub follow_up: u64,
    pub pending_questions: u64,
}

/// The fanout's pending `question` frames for the session (replayed on attach).
#[derive(Clone, Copy, Debug, Default)]
pub struct SessionStatusFanoutView {
    pub pending_questions: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingQueue {
    pub steering: u64,
    pub follow_up: u64,
}

/// The derived half of a `session_status` record.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DerivedSessionStatus {
    pub status: SessionStatusValue,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(serialize_with = "serialize_kind")]
    pub kind: SessionKind,
    pub cwd: String,
    pub attachments: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub durable_session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<SessionStatusModel>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_level: Option<String>,
    pub pending_queue: PendingQueue,
    pub context: PublishedSessionContext,
}

pub fn session_kind_name(kind: SessionKind) -> &'static str {
    match kind {
        SessionKind::Interactive => "interactive",
        SessionKind::Worker => "worker",
    }
}

fn serialize_kind<S: serde::Serializer>(
    kind: &SessionKind,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(session_kind_name(*kind))
}

fn live_status(
    runtime: &SessionStatusRuntimeView,
    fanout: SessionStatusFanoutView,
) -> SessionStatusValue {
    // A question is asked from inside a running turn, and compaction can run inside one too: the narrower
    // state wins, so a supervisor sees "waiting on the user" rather than a turn that merely looks busy.
    if fanout.pending_questions > 0 || runtime.pending_questions > 0 {
        SessionStatusValue::WaitingInput
    } else if runtime.is_compacting {
        SessionStatusValue::Compacting
    } else if runtime.is_streaming {
        SessionStatusValue::Running
    } else {
        SessionStatusValue::Idle
    }
}

fn status_of(
    entry: &SessionStatusEntryView,
    runtime: Option<&SessionStatusRuntimeView>,
    fanout: SessionStatusFanoutView,
    terminal: Option<&SessionStatusTerminal>,
) -> SessionStatusValue {
    if let Some(terminal) = terminal {
        return terminal.status;
    }
    match entry.state {
        RpcSessionState::Opening => SessionStatusValue::Opening,
        RpcSessionState::Closing | RpcSessionState::Quarantined | RpcSessionState::Closed => {
            SessionStatusValue::Closed
        }
        RpcSessionState::Open => match runtime {
            None => SessionStatusValue::Opening,
            Some(runtime) => live_status(runtime, fanout),
        },
    }
}

/// Derive a session's status. `terminal` is the seal the writer recorded (park, close, release); `cause`
/// is the change being published, used only for the `reason` of a run that an abort ended.
pub fn derive_session_status(
    entry: &SessionStatusEntryView,
    runtime: Option<&SessionStatusRuntimeView>,
    fanout: SessionStatusFanoutView,
    terminal: Option<&SessionStatusTerminal>,
    cause: Option<SessionStatusCause>,
) -> DerivedSessionStatus {
    let status = status_of(entry, runtime, fanout, terminal);
    let reason = terminal
        .and_then(|terminal| terminal.reason.clone())
        .or_else(|| {
            (status == SessionStatusValue::Idle && cause == Some(SessionStatusCause::Abort))
                .then(|| "aborted".to_owned())
        });
    DerivedSessionStatus {
        status,
        reason,
        kind: entry.kind,
        cwd: entry.cwd.clone(),
        attachments: entry.attachments.max(0) as u64,
        durable_session_id: entry.durable_session_id.clone(),
        session_path: entry.session_path.clone(),
        name: entry.name.clone(),
        model: runtime.and_then(|runtime| runtime.model.clone()),
        thinking_level: runtime.and_then(|runtime| runtime.thinking_level.clone()),
        pending_queue: PendingQueue {
            steering: runtime.map_or(0, |runtime| runtime.steering),
            follow_up: runtime.map_or(0, |runtime| runtime.follow_up),
        },
        context: entry.context.clone(),
    }
}

/// The terminal a lifecycle record seals a session with. A park and a handoff park leave a session that
/// reopens by path (`unloaded`); every other close ends it (`closed`) with `session_closed.reason`.
pub fn terminal_of_lifecycle(
    record_type: &str,
    reason: Option<&Value>,
) -> Option<SessionStatusTerminal> {
    let reason = reason.and_then(Value::as_str);
    if record_type == "session_parked" {
        return Some(SessionStatusTerminal {
            status: SessionStatusValue::Unloaded,
            reason: Some(
                if reason == Some("unloaded") {
                    "unload_session"
                } else {
                    "idle"
                }
                .to_owned(),
            ),
        });
    }
    if record_type != "session_closed" {
        return None;
    }
    if reason == Some("handoff_parked") {
        return Some(SessionStatusTerminal {
            status: SessionStatusValue::Unloaded,
            reason: Some("handoff".to_owned()),
        });
    }
    Some(SessionStatusTerminal {
        status: SessionStatusValue::Closed,
        reason: reason.map(str::to_owned),
    })
}

/// The status change a session record publishes, or `None` when the record changes no status.
pub fn cause_of_record(record: &Value) -> Option<SessionStatusCause> {
    use SessionStatusCause as Cause;
    match record.get("type").and_then(Value::as_str)? {
        "agent_start" => Some(Cause::Prompt),
        "agent_end" => Some(Cause::AgentEnd),
        "agent_idle" => Some(Cause::AgentIdle),
        "session_abort" => Some(Cause::Abort),
        "queue_update" => Some(Cause::Queue),
        "model_changed" | "thinking_level_changed" => Some(Cause::SetModel),
        "session_replaced" => Some(Cause::Replace),
        "session_parked" => Some(Cause::Park),
        "session_closed" => Some(Cause::Close),
        "question_resolved" => Some(Cause::Question),
        "extension_ui_request" => (record.get("method").and_then(Value::as_str)
            == Some("question"))
        .then_some(Cause::Question),
        "compaction_start" | "compaction_end" => Some(Cause::Compaction),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn entry(state: RpcSessionState) -> SessionStatusEntryView {
        SessionStatusEntryView {
            state,
            kind: SessionKind::Interactive,
            cwd: "/w".into(),
            attachments: -1,
            durable_session_id: None,
            session_path: Some("/w/s.jsonl".into()),
            name: None,
            context: BTreeMap::new(),
            open_context: None,
        }
    }

    #[test]
    fn derivation_table() {
        let none = SessionStatusFanoutView::default();
        let open = entry(RpcSessionState::Open);
        assert_eq!(
            derive_session_status(&entry(RpcSessionState::Opening), None, none, None, None).status,
            SessionStatusValue::Opening
        );
        assert_eq!(
            derive_session_status(&open, None, none, None, None).status,
            SessionStatusValue::Opening
        );
        assert_eq!(
            derive_session_status(&entry(RpcSessionState::Quarantined), None, none, None, None)
                .status,
            SessionStatusValue::Closed
        );
        let mut runtime = SessionStatusRuntimeView {
            is_streaming: true,
            is_compacting: true,
            ..Default::default()
        };
        assert_eq!(
            derive_session_status(&open, Some(&runtime), none, None, None).status,
            SessionStatusValue::Compacting
        );
        assert_eq!(
            derive_session_status(
                &open,
                Some(&runtime),
                SessionStatusFanoutView {
                    pending_questions: 1
                },
                None,
                None
            )
            .status,
            SessionStatusValue::WaitingInput
        );
        runtime.is_compacting = false;
        assert_eq!(
            derive_session_status(&open, Some(&runtime), none, None, None).status,
            SessionStatusValue::Running
        );
        runtime.is_streaming = false;
        let idle = derive_session_status(
            &open,
            Some(&runtime),
            none,
            None,
            Some(SessionStatusCause::Abort),
        );
        assert_eq!(
            (idle.status, idle.reason.as_deref(), idle.attachments),
            (SessionStatusValue::Idle, Some("aborted"), 0)
        );
        let wire = serde_json::to_value(&idle).expect("serialize");
        assert_eq!(wire["status"], "idle");
        assert_eq!(wire["kind"], "interactive");
        assert_eq!(wire["pendingQueue"], json!({"steering":0,"followUp":0}));
    }

    #[test]
    fn terminals_and_causes() {
        let parked =
            terminal_of_lifecycle("session_parked", Some(&json!("unloaded"))).expect("park");
        assert_eq!(
            (parked.status, parked.reason.as_deref()),
            (SessionStatusValue::Unloaded, Some("unload_session"))
        );
        let handoff = terminal_of_lifecycle("session_closed", Some(&json!("handoff_parked")))
            .expect("handoff");
        assert_eq!(handoff.reason.as_deref(), Some("handoff"));
        let closed = terminal_of_lifecycle("session_closed", None).expect("closed");
        assert_eq!(
            (closed.status, closed.reason),
            (SessionStatusValue::Closed, None)
        );
        assert!(terminal_of_lifecycle("agent_end", None).is_none());
        assert_eq!(
            cause_of_record(&json!({"type":"extension_ui_request","method":"question"})),
            Some(SessionStatusCause::Question)
        );
        assert_eq!(
            cause_of_record(&json!({"type":"extension_ui_request","method":"select"})),
            None
        );
        assert_eq!(
            cause_of_record(&json!({"type":"thinking_level_changed"})),
            Some(SessionStatusCause::SetModel)
        );
    }
}
