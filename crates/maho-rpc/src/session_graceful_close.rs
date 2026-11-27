//! Port of senpi `packages/coding-agent/src/modes/rpc/session-graceful-close.ts`.
//!
//! The drain of a graceful close (`unload_session { graceful: true }`, oh-my-openagent#14313).

use crate::session_release_interrupt::ReleaseDropped;
use maho_core::agent_session::AgentSession;
use maho_core::session_closed::{
    CLOSING_HOLD_KEY, SESSION_CLOSED_ENTRY, SessionClosedRecord, SessionClosedReturned,
    SessionClosedStopped,
};
use std::time::{Duration, Instant};

pub const GRACEFUL_CLOSE_DEFAULT_WAIT_MS: u64 = 60_000;
pub const GRACEFUL_CLOSE_MAX_WAIT_MS: u64 = 600_000;
pub const GRACEFUL_ADMISSION_CLOSED: &str =
    "session_closing: the session is closing on request and takes no new deliveries";

#[derive(Debug, Clone)]
pub struct GracefulDrain {
    pub dropped: ReleaseDropped,
    pub started_at: Instant,
}

pub trait GracefulDrainPort {
    fn busy(&self) -> Vec<String>;
    fn is_idle(&self) -> bool {
        self.busy().is_empty()
    }
}

pub fn graceful_wait_ms(requested: Option<u64>) -> u64 {
    requested
        .unwrap_or(GRACEFUL_CLOSE_DEFAULT_WAIT_MS)
        .min(GRACEFUL_CLOSE_MAX_WAIT_MS)
}

pub fn fence_for_close(session: &AgentSession) -> GracefulDrain {
    let _ = CLOSING_HOLD_KEY;
    session
        .external_admission()
        .close(GRACEFUL_ADMISSION_CLOSED.to_owned());
    let cleared = session.clear_queue(false);
    let mut user_messages = cleared.steering;
    user_messages.extend(cleared.follow_up);
    let dropped = ReleaseDropped {
        deliveries: Vec::new(),
        user_messages,
    };
    GracefulDrain {
        dropped,
        started_at: Instant::now(),
    }
}

pub fn reopen_after_drain(session: &AgentSession) {
    session.external_admission().reopen();
}

pub fn drain_facts(session: &AgentSession, drain: &GracefulDrain) -> (u64, u64) {
    (
        session.external_admission().turn_epoch(),
        drain.started_at.elapsed().as_millis() as u64,
    )
}

pub fn record_graceful_close(session: &AgentSession, drain: &GracefulDrain) -> SessionClosedRecord {
    let (turn, waited_ms) = drain_facts(session, drain);
    let record = SessionClosedRecord {
        graceful: true,
        turn,
        waited_ms,
        returned: SessionClosedReturned {
            deliveries: drain.dropped.deliveries.clone(),
            user_messages: drain.dropped.user_messages.clone(),
        },
        stopped: SessionClosedStopped {
            wake_sources: session.active_wake_sources(),
        },
        at: maho_core::session_manager::now_iso_millis(),
    };
    if let Ok(val) = serde_json::to_value(&record) {
        let _ = session
            .with_session_manager_mut(|sm| sm.append_custom(SESSION_CLOSED_ENTRY, Some(val)));
    }
    record
}

/// Awaits until `port.is_idle()` or `wait_ms` expires, waking via `notify` (event-driven, no polling).
pub async fn drain_until_quiet(
    port: &dyn GracefulDrainPort,
    notify: &tokio::sync::Notify,
    wait_ms: u64,
) -> bool {
    let deadline = tokio::time::Instant::now() + Duration::from_millis(wait_ms);
    loop {
        let notified = notify.notified();
        if port.is_idle() {
            return true;
        }
        let now = tokio::time::Instant::now();
        if now >= deadline {
            return false;
        }
        if tokio::time::timeout_at(deadline, notified).await.is_err() {
            return port.is_idle();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    struct MockPort(Arc<AtomicBool>);
    impl GracefulDrainPort for MockPort {
        fn busy(&self) -> Vec<String> {
            if self.0.load(Ordering::Acquire) {
                vec!["turn".to_owned()]
            } else {
                Vec::new()
            }
        }
    }

    #[tokio::test]
    async fn wait_caps_and_drain_wakes_on_notify() {
        assert_eq!(graceful_wait_ms(None), GRACEFUL_CLOSE_DEFAULT_WAIT_MS);
        assert_eq!(graceful_wait_ms(Some(999_999)), GRACEFUL_CLOSE_MAX_WAIT_MS);

        let busy = Arc::new(AtomicBool::new(true));
        let notify = Arc::new(tokio::sync::Notify::new());
        let port = MockPort(Arc::clone(&busy));
        let notify_tx = Arc::clone(&notify);
        let busy_tx = Arc::clone(&busy);
        tokio::spawn(async move {
            tokio::task::yield_now().await;
            busy_tx.store(false, Ordering::Release);
            notify_tx.notify_waiters();
        });
        assert!(drain_until_quiet(&port, &notify, 1000).await);
    }
}
