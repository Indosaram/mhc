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
        terminal: terminal.map(|v| {
            serde_json::from_value(serde_json::json!(v)).expect("validated child terminal")
        }),
        holds: holds
            .iter()
            .map(|v| serde_json::from_value(v.clone()).expect("validated lifecycle hold"))
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn typed_adapter_preserves_terminal_and_hold_precedence() {
        let waits = json!({"children":0,"background":[],"human":[]});
        let holds = vec![json!({"kind":"session_hold","since":"now"})];
        assert_eq!(
            lifecycle_state("root", None, &holds, &waits, false, false),
            "paused"
        );
        assert_eq!(
            lifecycle_state("child", Some("completed"), &[], &waits, true, false),
            "done"
        );
        assert_eq!(
            lifecycle_state("child", Some("interrupted"), &holds, &waits, false, true),
            "paused"
        );
    }
    #[test]
    fn pending_admission_keeps_root_running() {
        let waits = json!({"children":0,"background":[],"human":[]});
        assert_eq!(
            lifecycle_state("root", None, &[], &waits, true, false),
            "running"
        );
        assert_eq!(
            lifecycle_state("root", None, &[], &waits, false, false),
            "done"
        );
    }
}
