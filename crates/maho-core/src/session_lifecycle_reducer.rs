//! Lifecycle state derivation and terminal-state classification from lifecycle facts.
//!
//! Port of senpi `packages/coding-agent/src/core/session-lifecycle-reducer.ts`.

use crate::session_lifecycle_types::{
    ChildTerminal, LifecycleFailure, LifecycleHold, LifecycleHoldKind,
    LifecycleWaits, SessionLifecycleState, SessionRole,
};

#[derive(Clone, Debug, PartialEq)]
pub struct LifecycleFacts {
    pub role: SessionRole,
    pub terminal: Option<ChildTerminal>,
    pub holds: Vec<LifecycleHold>,
    pub waits: LifecycleWaits,
    pub running: bool,
    pub failure: Option<LifecycleFailure>,
}

/// Approved first-match precedence; no turn-end verdict label is an input.
pub fn lifecycle_state(facts: &LifecycleFacts) -> SessionLifecycleState {
    let stop_held = facts.holds.iter().any(|hold| {
        hold.kind == LifecycleHoldKind::SessionHold || hold.kind == LifecycleHoldKind::InheritedHold
    });

    if facts.role == SessionRole::Child && let Some(terminal) = facts.terminal {
        match terminal {
            ChildTerminal::Completed => {
                if facts.holds.is_empty() {
                    return SessionLifecycleState::Done;
                }
            }
            ChildTerminal::Cancelled => {
                return SessionLifecycleState::Cancelled;
            }
            ChildTerminal::Error | ChildTerminal::Lost => {
                return SessionLifecycleState::Failed;
            }
            ChildTerminal::Interrupted => {
                if !stop_held {
                    return SessionLifecycleState::Failed;
                }
            }
        }
    }

    if stop_held {
        return SessionLifecycleState::Paused;
    }
    if facts
        .holds
        .iter()
        .any(|hold| hold.kind == LifecycleHoldKind::RunFence)
        && !facts.running
    {
        return SessionLifecycleState::Paused;
    }
    if !facts.holds.is_empty()
        && facts
            .holds
            .iter()
            .all(|hold| hold.kind == LifecycleHoldKind::Unattended)
    {
        return SessionLifecycleState::Held;
    }
    if !facts.waits.human.is_empty() {
        return SessionLifecycleState::WaitingHuman;
    }
    if facts.running {
        return SessionLifecycleState::Running;
    }
    if facts.waits.children > 0 {
        return SessionLifecycleState::WaitingChildren;
    }
    if !facts.waits.background.is_empty() {
        return SessionLifecycleState::WaitingBackground;
    }
    // The task record, not the child's local idle, owns its terminal outcome.
    if facts.role == SessionRole::Child {
        return SessionLifecycleState::Running;
    }
    if facts.failure.is_some() {
        return SessionLifecycleState::Failed;
    }
    SessionLifecycleState::Done
}

pub fn lifecycle_terminal(state: SessionLifecycleState) -> bool {
    match state {
        SessionLifecycleState::Done
        | SessionLifecycleState::Failed
        | SessionLifecycleState::Cancelled => true,
        SessionLifecycleState::Running
        | SessionLifecycleState::WaitingChildren
        | SessionLifecycleState::WaitingBackground
        | SessionLifecycleState::WaitingHuman
        | SessionLifecycleState::Paused
        | SessionLifecycleState::Held => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_lifecycle_types::{
        BackgroundWait, FailureReason, HumanWait, HumanWaitKind, LifecycleFailure,
    };

    fn default_facts() -> LifecycleFacts {
        LifecycleFacts {
            role: SessionRole::Root,
            terminal: None,
            holds: Vec::new(),
            waits: LifecycleWaits::default(),
            running: false,
            failure: None,
        }
    }

    #[test]
    fn root_idle_without_failure_is_done() {
        let facts = default_facts();
        assert_eq!(lifecycle_state(&facts), SessionLifecycleState::Done);
        assert!(lifecycle_terminal(SessionLifecycleState::Done));
    }

    #[test]
    fn root_idle_with_failure_is_failed() {
        let mut facts = default_facts();
        facts.failure = Some(LifecycleFailure {
            reason: FailureReason::ProviderError,
            message: Some("boom".into()),
            retry_at_ms: None,
            task_id: None,
            run_epoch: None,
        });
        assert_eq!(lifecycle_state(&facts), SessionLifecycleState::Failed);
        assert!(lifecycle_terminal(SessionLifecycleState::Failed));
    }

    #[test]
    fn stop_held_precedence_is_paused() {
        let mut facts = default_facts();
        facts.holds.push(LifecycleHold {
            kind: LifecycleHoldKind::SessionHold,
            since: "2026-01-01T00:00:00Z".into(),
        });
        facts.running = true;
        assert_eq!(lifecycle_state(&facts), SessionLifecycleState::Paused);
        assert!(!lifecycle_terminal(SessionLifecycleState::Paused));
    }

    #[test]
    fn waiting_human_outranks_running() {
        let mut facts = default_facts();
        facts.running = true;
        facts.waits.human.push(HumanWait {
            kind: HumanWaitKind::Question,
            origin_session_id: "s1".into(),
            request_id: "r1".into(),
            revision: 1,
            child_session_id: None,
            child_label: None,
            since: "2026-01-01T00:00:00Z".into(),
        });
        assert_eq!(lifecycle_state(&facts), SessionLifecycleState::WaitingHuman);
    }

    #[test]
    fn waiting_children_and_background() {
        let mut facts = default_facts();
        facts.waits.children = 2;
        assert_eq!(
            lifecycle_state(&facts),
            SessionLifecycleState::WaitingChildren
        );

        facts.waits.children = 0;
        facts.waits.background.push(BackgroundWait {
            source: "timer".into(),
            active_count: 1,
            retry_at_ms: None,
        });
        assert_eq!(
            lifecycle_state(&facts),
            SessionLifecycleState::WaitingBackground
        );
    }

    #[test]
    fn child_terminal_resolution() {
        let mut facts = default_facts();
        facts.role = SessionRole::Child;
        facts.terminal = Some(ChildTerminal::Completed);
        assert_eq!(lifecycle_state(&facts), SessionLifecycleState::Done);

        facts.terminal = Some(ChildTerminal::Cancelled);
        assert_eq!(lifecycle_state(&facts), SessionLifecycleState::Cancelled);

        facts.terminal = Some(ChildTerminal::Error);
        assert_eq!(lifecycle_state(&facts), SessionLifecycleState::Failed);

        facts.terminal = Some(ChildTerminal::Interrupted);
        assert_eq!(lifecycle_state(&facts), SessionLifecycleState::Failed);

        facts.holds.push(LifecycleHold {
            kind: LifecycleHoldKind::SessionHold,
            since: "now".into(),
        });
        assert_eq!(lifecycle_state(&facts), SessionLifecycleState::Paused);
    }
}
