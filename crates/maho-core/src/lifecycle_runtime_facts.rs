//! Live lifecycle facts, retaining attention even beneath a hold.
use crate::{agent_session::AgentSession, session_activity::SessionActivitySnapshot};
use serde_json::Value;
pub struct LifecycleRuntimeFacts {
    pub activity: SessionActivitySnapshot,
    pub pending_messages: usize,
    pub holds: Vec<Value>,
    pub human: Vec<Value>,
    pub input_admission_pending: bool,
}
pub fn read_lifecycle_runtime_facts(
    session: &AgentSession,
    human: Vec<Value>,
) -> LifecycleRuntimeFacts {
    LifecycleRuntimeFacts {
        activity: session.activity_snapshot(),
        pending_messages: session.pending_message_count(),
        holds: session
            .holds()
            .into_iter()
            .map(|h| serde_json::json!({"kind":h.kind.as_str(),"since":h.since}))
            .collect(),
        human,
        input_admission_pending: session.external_admission().has_input_work(),
    }
}

/// Adapt durable JSON facts to the shared typed reducer (one precedence definition).
pub fn lifecycle_state(
    role: &str,
    terminal: Option<&str>,
    holds: &[serde_json::Value],
    waits: &serde_json::Value,
    running: bool,
    failure: bool,
) -> String {
    use crate::session_lifecycle_types::*;
    let facts = crate::session_lifecycle_reducer::LifecycleFacts {
        role: if role == "child" {
            SessionRole::Child
        } else {
            SessionRole::Root
        },
        terminal: terminal.and_then(|v| serde_json::from_value(serde_json::json!(v)).ok()),
        holds: holds
            .iter()
            .filter_map(|v| serde_json::from_value(v.clone()).ok())
            .collect(),
        waits: serde_json::from_value(waits.clone()).expect("validated lifecycle waits"),
        running,
        failure: failure.then_some(LifecycleFailure {
            reason: FailureReason::ProviderError,
            message: None,
            retry_at_ms: None,
            task_id: None,
            run_epoch: None,
        }),
    };
    serde_json::to_value(crate::session_lifecycle_reducer::lifecycle_state(&facts))
        .expect("lifecycle state")
        .as_str()
        .expect("state string")
        .to_owned()
}
