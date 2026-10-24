//! senpi `tool-child/flow-control.ts`.
use std::{
    collections::HashMap,
    sync::{Mutex, MutexGuard, PoisonError},
};

use tokio::sync::oneshot;

use crate::protocol::{LOSSY_FRAME_TYPES, TOOL_CHILD_LIMITS, ToolChildMessage};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WakeAdmissionError {
    pub bytes: usize,
}

impl std::fmt::Display for WakeAdmissionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "wake_too_large: {} bytes exceeds the {}-byte pending budget",
            self.bytes, TOOL_CHILD_LIMITS.wake_pending_bytes
        )
    }
}

impl std::error::Error for WakeAdmissionError {}

impl WakeAdmissionError {
    pub const CODE: &'static str = "wake_too_large";
    pub const NAME: &'static str = "WakeAdmissionError";

    pub fn new(bytes: usize) -> Self {
        Self { bytes }
    }

    pub fn code(&self) -> &'static str {
        Self::CODE
    }
}

struct BlockedWake {
    wake_id: String,
    bytes: usize,
    release: oneshot::Sender<()>,
}

#[derive(Default)]
struct WakeState {
    pending: HashMap<String, usize>,
    blocked: Vec<BlockedWake>,
    pending_bytes: usize,
}

impl WakeState {
    fn fits(&self, bytes: usize) -> bool {
        self.pending.len() < TOOL_CHILD_LIMITS.wake_pending
            && self.pending_bytes + bytes <= TOOL_CHILD_LIMITS.wake_pending_bytes
    }

    fn admit(&mut self, wake_id: String, bytes: usize) {
        self.pending.insert(wake_id, bytes);
        self.pending_bytes += bytes;
    }
}

/// Wake notifications awaiting `wake_ack`, bounded by count and summed bytes. A wake past either bound waits, FIFO,
/// until an ack frees room: wakes are durable wake-source demand and are never dropped.
#[derive(Default)]
pub struct WakeAdmission {
    state: Mutex<WakeState>,
}

impl WakeAdmission {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> MutexGuard<'_, WakeState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn pending(&self) -> usize {
        self.lock().pending.len()
    }

    pub fn blocked(&self) -> usize {
        self.lock().blocked.len()
    }

    pub async fn send(&self, wake_id: &str, bytes: usize) -> Result<(), WakeAdmissionError> {
        let waiting = {
            let mut state = self.lock();
            if bytes > TOOL_CHILD_LIMITS.wake_pending_bytes {
                return Err(WakeAdmissionError::new(bytes));
            }
            if state.blocked.is_empty() && state.fits(bytes) {
                state.admit(wake_id.to_owned(), bytes);
                return Ok(());
            }
            let (release, waiting) = oneshot::channel();
            state.blocked.push(BlockedWake {
                wake_id: wake_id.to_owned(),
                bytes,
                release,
            });
            waiting
        };
        // A dropped sender means the admission itself was dropped; nothing is left to wait for.
        let _ = waiting.await;
        Ok(())
    }

    pub fn ack(&self, wake_id: &str) {
        let mut state = self.lock();
        let Some(bytes) = state.pending.remove(wake_id) else {
            return;
        };
        state.pending_bytes -= bytes;
        while state
            .blocked
            .first()
            .is_some_and(|next| state.fits(next.bytes))
        {
            let next = state.blocked.remove(0);
            state.admit(next.wake_id, next.bytes);
            let _ = next.release.send(());
        }
    }
}

/// Per-second budget for `trace` frames, the only lossy class: an over-budget frame is dropped and counted.
#[derive(Debug, Default)]
pub struct TraceBudget {
    second: Option<u64>,
    admitted: usize,
    dropped: usize,
}

impl TraceBudget {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn dropped(&self) -> usize {
        self.dropped
    }

    pub fn admit(&mut self, _frame: &ToolChildMessage, now_ms: u64) -> bool {
        let second = now_ms / 1000;
        if self.second != Some(second) {
            self.second = Some(second);
            self.admitted = 0;
        }
        if self.admitted >= TOOL_CHILD_LIMITS.trace_per_second {
            self.dropped += 1;
            return false;
        }
        self.admitted += 1;
        true
    }
}

pub fn is_lossy(message: &ToolChildMessage) -> bool {
    LOSSY_FRAME_TYPES.contains(&message.message_type())
}

#[cfg(test)]
mod tests {
    use futures::poll;
    use serde_json::json;

    use super::*;

    fn message(value: serde_json::Value) -> ToolChildMessage {
        serde_json::from_value(value).expect("valid tool-child message")
    }

    #[tokio::test]
    async fn the_65th_pending_wake_waits_for_wake_ack_and_is_never_dropped() {
        let wakes = WakeAdmission::new();
        for index in 1..=64 {
            assert!(wakes.send(&format!("w-{index}"), 10).await.is_ok());
        }
        let mut sixty_fifth = Box::pin(wakes.send("w-65", 10));
        let mut sixty_sixth = Box::pin(wakes.send("w-66", 10));
        assert!(poll!(sixty_fifth.as_mut()).is_pending());
        assert!(poll!(sixty_sixth.as_mut()).is_pending());
        assert_eq!(wakes.blocked(), 2);
        assert_eq!(wakes.pending(), 64);
        wakes.ack("w-1");
        assert!(poll!(sixty_fifth.as_mut()).is_ready());
        assert!(poll!(sixty_sixth.as_mut()).is_pending());
        wakes.ack("w-2");
        assert!(poll!(sixty_sixth.as_mut()).is_ready());
        assert_eq!(wakes.pending(), 64);
        assert_eq!(wakes.blocked(), 0);
    }

    #[tokio::test]
    async fn bounds_pending_wakes_by_summed_bytes_and_refuses_one_larger_than_the_whole_budget() {
        let wakes = WakeAdmission::new();
        let part = 600 * 1024;
        assert!(wakes.send("a", part).await.is_ok());
        let mut second = Box::pin(wakes.send("b", part));
        assert!(poll!(second.as_mut()).is_pending());
        wakes.ack("a");
        assert!(poll!(second.as_mut()).is_ready());
        let huge = wakes
            .send("huge", TOOL_CHILD_LIMITS.wake_pending_bytes + 1)
            .await;
        assert_eq!(
            huge,
            Err(WakeAdmissionError::new(
                TOOL_CHILD_LIMITS.wake_pending_bytes + 1
            ))
        );
        assert_eq!(WakeAdmissionError::new(5).code(), "wake_too_large");
    }

    #[test]
    fn drops_and_counts_the_1025th_trace_frame_in_one_second_then_admits_again_in_the_next() {
        let mut budget = TraceBudget::new();
        let frame = message(json!({ "type": "trace", "atMs": 0, "event": "tick" }));
        let admitted: Vec<bool> = (0..1025).map(|_| budget.admit(&frame, 5_000)).collect();
        assert_eq!(admitted.iter().filter(|admitted| **admitted).count(), 1024);
        assert_eq!(admitted.last(), Some(&false));
        assert_eq!(budget.dropped(), 1);
        assert!(budget.admit(&frame, 6_000));
        assert_eq!(budget.dropped(), 1);
    }

    #[test]
    fn trace_is_the_only_lossy_class_results_proofs_and_ticks_are_never_lossy() {
        assert_eq!(LOSSY_FRAME_TYPES.len(), 1);
        assert!(is_lossy(&message(
            json!({ "type": "trace", "atMs": 1, "event": "e" })
        )));
        assert!(!is_lossy(&message(
            json!({ "type": "launch_proof", "launchId": "l", "ackTick": 1, "invokeTick": 2 })
        )));
        assert!(!is_lossy(&message(
            json!({ "type": "result", "jobId": "j", "value": 1 })
        )));
        assert!(!is_lossy(&message(json!({
            "type": "slot_state", "launchId": "l", "ackTick": 1, "invokeTick": 2, "invocations": 1
        }))));
    }
}
