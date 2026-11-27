//! Port of senpi `packages/agent/src/stream-keepalive.ts`.
//!
//! Transport liveness for one provider request (senpi#4608).

use std::sync::{Arc, Mutex};

use crate::assistant_terminal_state::now_ms;

/// The wording keeps "timed out" so the retryable-error classifier treats it like the other stream bounds.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "Provider stream sent only keep-alives and timed out after {timeout_ms}ms (raise streamKeepAliveTimeoutMs: retry.provider.keepAliveTimeoutMs in senpi settings; 0 disables)"
)]
pub struct StreamKeepAliveTimeoutError {
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SilenceDeadlineVerdict {
    Expire,
    Extend { delay_ms: u64 },
    KeepAliveExhausted(StreamKeepAliveTimeoutError),
}

#[derive(Clone)]
pub struct TransportLiveness {
    keep_alive_timeout_ms: Option<u64>,
    state: Arc<Mutex<LivenessState>>,
}

struct LivenessState {
    last_activity_at: Option<i64>,
    content_silence_since: i64,
}

impl TransportLiveness {
    pub fn note_activity(&self) {
        self.state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .last_activity_at = Some(now_ms());
    }

    pub fn content_arrived(&self) {
        self.state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .content_silence_since = now_ms();
    }

    pub fn on_silence_deadline(&self, window_ms: u64) -> SilenceDeadlineVerdict {
        let now = now_ms();
        let guard = self.state.lock().unwrap_or_else(|p| p.into_inner());
        let Some(last_activity) = guard.last_activity_at else {
            return SilenceDeadlineVerdict::Expire;
        };
        let elapsed_since_activity = now.saturating_sub(last_activity).max(0) as u64;
        if elapsed_since_activity >= window_ms {
            return SilenceDeadlineVerdict::Expire;
        }
        let delay_ms = window_ms - elapsed_since_activity;
        let Some(keep_alive_timeout_ms) = self.keep_alive_timeout_ms else {
            return SilenceDeadlineVerdict::Extend { delay_ms };
        };
        let content_silence = now.saturating_sub(guard.content_silence_since).max(0) as u64;
        if content_silence >= keep_alive_timeout_ms {
            return SilenceDeadlineVerdict::KeepAliveExhausted(StreamKeepAliveTimeoutError {
                timeout_ms: keep_alive_timeout_ms,
            });
        }
        let keep_alive_left_ms = keep_alive_timeout_ms - content_silence;
        SilenceDeadlineVerdict::Extend {
            delay_ms: delay_ms.min(keep_alive_left_ms).max(1),
        }
    }
}

pub fn create_transport_liveness(keep_alive_timeout_ms: Option<u64>) -> TransportLiveness {
    TransportLiveness {
        keep_alive_timeout_ms: keep_alive_timeout_ms.filter(|ms| *ms > 0),
        state: Arc::new(Mutex::new(LivenessState {
            last_activity_at: None,
            content_silence_since: now_ms(),
        })),
    }
}
