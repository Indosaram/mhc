//! Port of omo-senpi `components/task/skill-invocation-handoff.ts`: session-keyed plan-gate
//! evidence handed from a retiring extension generation to its replacement. `reload` rebuilds the
//! runner for the same session; `new`/`resume`/`fork` set the session aside under the same id.
//! `quit` retires it. The store is process-global so both generations share it.
use std::sync::{Mutex, PoisonError};

use maho_ext_api::SessionReason;
use senpi_task::agents::PlanArtifactReference;

/// Oldest-first eviction bound.
pub const SESSION_HANDOFF_CAPACITY: usize = 256;

#[derive(Clone, Debug, Default)]
pub struct SessionHandoff {
    pub invoked: Vec<String>,
    pub requested: Vec<String>,
    pub plan_touches: Vec<PlanArtifactReference>,
    pub plan_touch_sequence: u64,
}

/// Insertion-ordered so the oldest deposit is evicted first.
static SESSION_HANDOFFS: Mutex<Vec<(String, SessionHandoff)>> = Mutex::new(Vec::new());

pub fn shutdown_keeps_session(reason: SessionReason) -> bool {
    matches!(
        reason,
        SessionReason::Reload | SessionReason::New | SessionReason::Resume | SessionReason::Fork
    )
}

pub fn deposit_session_handoff(session: &str, handoff: SessionHandoff) {
    let mut store = SESSION_HANDOFFS
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    store.retain(|(id, _)| id != session);
    if handoff.invoked.is_empty() && handoff.requested.is_empty() && handoff.plan_touches.is_empty()
    {
        return;
    }
    store.push((session.to_owned(), handoff));
    while store.len() > SESSION_HANDOFF_CAPACITY {
        store.remove(0);
    }
}

pub fn take_session_handoff(session: &str) -> Option<SessionHandoff> {
    let mut store = SESSION_HANDOFFS
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let index = store.iter().position(|(id, _)| id == session)?;
    Some(store.remove(index).1)
}

pub fn discard_session_handoff(session: &str) {
    SESSION_HANDOFFS
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .retain(|(id, _)| id != session);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn handoff(skill: &str) -> SessionHandoff {
        SessionHandoff {
            invoked: vec![skill.to_owned()],
            ..Default::default()
        }
    }

    #[test]
    fn keeps_only_surviving_reasons() {
        assert!(shutdown_keeps_session(SessionReason::Reload));
        assert!(shutdown_keeps_session(SessionReason::Fork));
        assert!(!shutdown_keeps_session(SessionReason::Quit));
        assert!(!shutdown_keeps_session(SessionReason::Startup));
    }

    #[test]
    fn deposit_take_discard_and_empty() {
        deposit_session_handoff("handoff-a", handoff("x"));
        assert_eq!(
            take_session_handoff("handoff-a").map(|h| h.invoked),
            Some(vec!["x".to_owned()])
        );
        assert!(take_session_handoff("handoff-a").is_none());
        deposit_session_handoff("handoff-b", handoff("y"));
        discard_session_handoff("handoff-b");
        assert!(take_session_handoff("handoff-b").is_none());
        deposit_session_handoff("handoff-c", SessionHandoff::default());
        assert!(take_session_handoff("handoff-c").is_none());
    }

    #[test]
    fn evicts_oldest_over_capacity() {
        for i in 0..=SESSION_HANDOFF_CAPACITY {
            deposit_session_handoff(&format!("cap-{i}"), handoff("s"));
        }
        assert!(take_session_handoff("cap-0").is_none());
        assert!(take_session_handoff(&format!("cap-{SESSION_HANDOFF_CAPACITY}")).is_some());
        for i in 1..SESSION_HANDOFF_CAPACITY {
            discard_session_handoff(&format!("cap-{i}"));
        }
    }
}
