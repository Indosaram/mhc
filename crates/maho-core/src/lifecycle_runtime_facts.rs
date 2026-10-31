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
        holds: lifecycle_holds(&session.holds()),
        human,
        input_admission_pending: session.external_admission().has_input_work(),
    }
}

/// The lifecycle projection of the engine holds (`readLifecycleRuntimeFacts` holds).
pub fn lifecycle_holds(holds: &[crate::engine_turn_hold::SessionHold]) -> Vec<Value> {
    holds
        .iter()
        // The interruption gate is a human wait, not a user Stop. Preserve unattended and real Stop precedence.
        .filter(|h| {
            !(h.kind == crate::engine_turn_hold::HoldKind::SessionHold
                && h.key == crate::engine_turn_hold::INTERRUPTED_RUN_HOLD_KEY)
        })
        .map(|h| serde_json::json!({"kind":h.kind.as_str(),"since":h.since}))
        .collect()
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
    fn hold(
        kind: crate::engine_turn_hold::HoldKind,
        key: &str,
    ) -> crate::engine_turn_hold::SessionHold {
        crate::engine_turn_hold::SessionHold {
            kind,
            key: key.to_owned(),
            since: "now".to_owned(),
            data: None,
            source_counts: Default::default(),
        }
    }
    /// senpi test/suite/regressions/4204-interrupted-resume.test.ts: the interrupted-run session hold is a
    /// human wait, so it never projects as a lifecycle hold (a Stop), while a real Stop still does.
    #[test]
    fn the_interrupted_run_hold_is_not_a_lifecycle_hold() {
        use crate::engine_turn_hold::{HoldKind, INTERRUPTED_RUN_HOLD_KEY};
        let waits = json!({"children":0,"background":[],"human":[{"kind":"interrupted_run","originSessionId":"s","requestId":"interrupted:1","revision":1,"since":"now"}]});
        let interrupted = lifecycle_holds(&[hold(HoldKind::SessionHold, INTERRUPTED_RUN_HOLD_KEY)]);
        assert!(interrupted.is_empty());
        assert_eq!(
            lifecycle_state("root", None, &interrupted, &waits, false, false),
            "waiting_human"
        );
        let stopped = lifecycle_holds(&[
            hold(HoldKind::SessionHold, INTERRUPTED_RUN_HOLD_KEY),
            hold(HoldKind::SessionHold, "stop"),
        ]);
        assert_eq!(stopped, vec![json!({"kind":"session_hold","since":"now"})]);
        assert_eq!(
            lifecycle_state("root", None, &stopped, &waits, false, false),
            "paused"
        );
        // Only the session_hold kind carries the gate; another kind under the same key still holds.
        assert_eq!(
            lifecycle_holds(&[hold(HoldKind::Unattended, INTERRUPTED_RUN_HOLD_KEY)]).len(),
            1
        );
    }
}
