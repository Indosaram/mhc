//! Resident Kibitzer public contract (latest `kibitzer/sidecar-contract.ts`).
//!
//! SUPERSEDES `/tmp/pc.txt`: the wake future is `KibitzerWakeResult` (structured status + nudges),
//! never a bare `Vec<RecallNudge>`, so an empty vector cannot hide a failure or a cancellation.
//! The child handle types live in `kibitzer_child.rs`; this module owns the sidecar constants,
//! state/outcome types and the wake-runner trait.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use memory_core::recall::RecallNudge;

use crate::recall_consumer::CollectedRecallCandidates;

pub use crate::kibitzer_child::{JudgeSettle, KibitzerChild, KibitzerChildSpawnInput, KibitzerChildSpawner, KibitzerChildObservation};

pub const KIBITZER_WAKE_TOOL_BUDGET: usize = 8;
pub const KIBITZER_WAKE_DEADLINE_MS: i64 = 90_000;
pub const KIBITZER_WAKE_MAX_TOTAL_MS: i64 = 300_000;
pub const KIBITZER_SIDECAR_MAX_TOKENS: i64 = 48_000;
pub const KIBITZER_RESEED_FRACTION: f64 = 0.6;
pub const KIBITZER_REJECTED_AFTER_WAKES: usize = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KibitzerSidecarState { Idle, TurnRunning, Reseeding, Backoff, Disposed }

/// A plain warning sink (`options.logger?.warn`); absent means silent.
pub type WarnFn = Arc<dyn Fn(&str) + Send + Sync>;

/// Injectable timers: production uses the runtime's unref'd timers; tests fire them by hand.
pub trait KibitzerSidecarTimers: Send + Sync {
    fn set(&self, callback: Box<dyn FnOnce() + Send>, ms: i64) -> u64;
    fn clear(&self, handle: u64);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KibitzerBufferedReason { MaxItemsZero, NoCandidates, NoNewCandidate, Cooldown, Backoff, SlotBusy, Disposed }

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KibitzerOfferResult {
    Seeded { wake: u64 },
    FollowedUp { wake: u64 },
    Steered { wake: u64 },
    Buffered { reason: KibitzerBufferedReason },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KibitzerWakeStatus { Completed, Empty, Failed, Dropped, Cancelled }

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KibitzerWakeOutcome {
    pub wake: u64,
    pub generation: u64,
    pub status: KibitzerWakeStatus,
    pub reason: Option<String>,
    pub model: Option<String>,
    pub slot_wait_ms: i64,
}

/// The structured result of ONE wake: nudges accepted this turn PLUS the settled status, so an empty
/// nudge vector never has to stand in for a failure or a cancellation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KibitzerWakeResult {
    pub nudges: Vec<RecallNudge>,
    pub status: KibitzerWakeStatus,
}

/// The settlement future the runner returns; the caller drives it on a background executor.
pub type KibitzerWakeFuture = Pin<Box<dyn Future<Output = KibitzerWakeResult> + Send>>;

pub type KibitzerWakeSpawn = Arc<dyn Fn(Pin<Box<dyn Future<Output = ()> + Send>>) + Send + Sync>;

/// One wake request: the sidecar owns the lease/deadline; the runner owns the child turn.
pub struct KibitzerWakeRequest {
    pub session_id: String,
    pub candidates: CollectedRecallCandidates,
    /// True once the deadline fired or the session shut down.
    pub cancel: Arc<dyn Fn() -> bool + Send + Sync>,
    /// `memory.recall.tool_budget`: child tool calls one wake may spend before it is cut off.
    pub max_tool_budget: usize,
}

/// The wake runner seam. `start` is NON-blocking: it subscribes to child events/nudges BEFORE
/// triggering the turn, triggers it, and returns the settlement future. No default implementation.
pub trait KibitzerWakeRunner: Send + Sync {
    fn start(&self, request: KibitzerWakeRequest) -> KibitzerWakeFuture;
    /// Wakes the actual child's current turn (deadline / budget / shutdown).
    fn abort(&self, session_id: &str);
    fn dispose(&self, session_id: &str);
    /// Disposes the resident child so the next wake recreates it from the reseed envelope.
    fn request_reseed(&self, session_id: &str);
}

/// One wake's tool-call budget, shared by its tool closures without a dependency on the host.
pub trait KibitzerToolBudget: Send + Sync {
    fn limit(&self) -> usize;
    fn used(&self) -> usize;
    /// Charges one call; an exhausted budget refuses the call without incrementing the count.
    fn charge(&self) -> bool;
}

/// The sidecar resets this slot on admission; tools resolve its current budget on each call.
pub trait KibitzerBudgetSlot: Send + Sync {
    fn current(&self) -> Arc<dyn KibitzerToolBudget>;
    fn reset(&self, limit: usize);
}
