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
        terminal: terminal.and_then(|v| serde_json::from_value(serde_json::json!(v)).ok()),
        holds: holds
            .iter()
            .filter_map(|v| serde_json::from_value(v.clone()).ok())
            .collect(),
        waits: serde_json::from_value(waits.clone()).unwrap_or_default(),
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
        .ok()
        .and_then(|state| state.as_str().map(str::to_owned))
        .unwrap_or_else(|| "done".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lifecycle_record::read_waits;
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
    /// A durable record written by the TypeScript side carries integral numbers as `f64`, so `retryAtMs` can arrive
    /// as `1000.0`. Reading it must not abort the process (macOS crashes 2026-11-15/16: a panic during a double panic
    /// in a destructor) and the projection must carry a plain integer.
    #[test]
    fn float_retry_timestamps_do_not_abort_the_projection() {
        let waits = read_waits(&json!({
            "children": 1.0,
            "background": [{"source": "monitor", "activeCount": 1.0, "retryAtMs": 1000.0}],
            "human": []
        }))
        .expect("waits with float counts");
        assert_eq!(waits["children"], json!(1));
        assert_eq!(waits["background"][0]["activeCount"], json!(1));
        assert_eq!(waits["background"][0]["retryAtMs"], json!(1000));
        // An outstanding child wait outranks a failure, so this only proves the read did not abort.
        assert_eq!(
            lifecycle_state("root", None, &[], &waits, false, true),
            "waiting_children"
        );
        // With no children left, the retry wait itself is what the projection reports.
        let retrying = read_waits(&json!({
            "children": 0.0,
            "background": [{"source": "provider-retry", "activeCount": 1.0, "retryAtMs": 1000.0}],
            "human": []
        }))
        .expect("waits with float retry");
        assert_eq!(retrying["children"], json!(0));
        assert_eq!(retrying["background"][0]["retryAtMs"], json!(1000));
        assert_eq!(
            lifecycle_state("root", None, &[], &retrying, false, true),
            "waiting_background"
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
