//! How one resident wake ended (latest `kibitzer/sidecar-outcome.ts`).
//!
//! A wake is one child turn: the seed or follow-up that started it plus every steer the turn
//! absorbed. The sidecar aborts a turn itself for three reasons (the tool budget, the wake
//! deadline, session shutdown), and none of them is a child failure: only a turn the engine settled
//! as an error, or a child that could not be started, counts toward the diagnostic streak that
//! eventually raises the `omo-kibitzer:gate` notice - with one exception: a start refused because
//! the pinned recall category cannot serve a model (`category_unavailable`, or a resolution that
//! only exists beyond the category, which the advisor refuses) is a permanent CONFIGURATION state,
//! not a transient failure. It is reported non-diagnostically and answered with one actionable
//! `omo-kibitzer:unavailable` notice per session instead of the streak.
//!
//! This module owns the closed wake types and their classification only. It reuses
//! `kibitzer_judge_outcome::classify_judge_turn` and `normalize_gate_reason` and never forks their
//! validation or secret predicate. Reporting IO, timers, admission and provider event transport
//! live in their own modules.

use memory_core::recall::RecallNudge;

use crate::kibitzer_child::JudgeSettle;
use crate::kibitzer_judge_outcome::{
    JudgeFailureCause, JudgeTurnClassification, classify_judge_turn, normalize_gate_reason,
};

/// The sidecar's own reasons for aborting a running turn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KibitzerWakeAbort {
    ToolBudget,
    Deadline,
    Shutdown,
}

/// Why a child turn failed, or a child that never started.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KibitzerWakeFailureCause {
    ChildFailed,
    ChildFailedUpstream,
    StartFailed,
}

/// Why the pinned recall category cannot serve a model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KibitzerWakeConfigurationCause {
    CategoryUnavailable,
    BeyondCategory,
}

/// Why the pinned recall category cannot serve a model, and which providers a connection would fix.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KibitzerWakeConfiguration {
    pub category: String,
    pub cause: KibitzerWakeConfigurationCause,
    pub missing_providers: Option<Vec<String>>,
}

/// How one wake ended (`KibitzerWakeEnd`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KibitzerWakeEnd {
    /// The engine settled the turn on its own; `nudges` on the outcome says whether it spoke.
    Completed,
    /// The sidecar aborted at the per-wake tool budget. Accepted nudges are kept.
    ToolBudgetExceeded,
    /// The sidecar aborted at the wake deadline. Accepted nudges are kept.
    Deadline,
    /// The child turn failed or the child could not be started; the sidecar backs off.
    Failed {
        cause: KibitzerWakeFailureCause,
        reason: Option<String>,
        /// Present when the start refusal is a configuration state, never a transient failure.
        configuration: Option<KibitzerWakeConfiguration>,
    },
    /// The main session shut down under the turn. Nothing is delivered.
    Cancelled,
}

impl KibitzerWakeEnd {
    /// `KibitzerWakeEnd["status"]`.
    pub fn status(&self) -> KibitzerWakeStatus {
        match self {
            KibitzerWakeEnd::Completed => KibitzerWakeStatus::Completed,
            KibitzerWakeEnd::ToolBudgetExceeded => KibitzerWakeStatus::ToolBudgetExceeded,
            KibitzerWakeEnd::Deadline => KibitzerWakeStatus::Deadline,
            KibitzerWakeEnd::Failed { .. } => KibitzerWakeStatus::Failed,
            KibitzerWakeEnd::Cancelled => KibitzerWakeStatus::Cancelled,
        }
    }

    /// The outcome's `cause`: a failure cause, or `shutdown` for a cancelled wake.
    pub fn cause(&self) -> Option<KibitzerWakeCause> {
        match self {
            KibitzerWakeEnd::Failed { cause, .. } => Some((*cause).into()),
            KibitzerWakeEnd::Cancelled => Some(KibitzerWakeCause::Shutdown),
            KibitzerWakeEnd::Completed | KibitzerWakeEnd::ToolBudgetExceeded | KibitzerWakeEnd::Deadline => None,
        }
    }
}

/// `KibitzerWakeEnd["status"]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KibitzerWakeStatus {
    Completed,
    ToolBudgetExceeded,
    Deadline,
    Failed,
    Cancelled,
}

/// The outcome's `cause`: a `KibitzerWakeFailureCause`, or the cancellation cause `shutdown`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KibitzerWakeCause {
    ChildFailed,
    ChildFailedUpstream,
    StartFailed,
    Shutdown,
}

impl From<KibitzerWakeFailureCause> for KibitzerWakeCause {
    fn from(cause: KibitzerWakeFailureCause) -> Self {
        match cause {
            KibitzerWakeFailureCause::ChildFailed => KibitzerWakeCause::ChildFailed,
            KibitzerWakeFailureCause::ChildFailedUpstream => KibitzerWakeCause::ChildFailedUpstream,
            KibitzerWakeFailureCause::StartFailed => KibitzerWakeCause::StartFailed,
        }
    }
}

/// Provider usage the wake's assistant messages reported, summed over the turn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KibitzerWakeUsage {
    pub input: i64,
    pub output: i64,
    pub cache_read: i64,
    pub cache_write: i64,
}

/// The parent cursor span of the events a wake carried.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KibitzerCursorSpan {
    pub first: usize,
    pub last: usize,
}

/// What `observe.ts` writes as one line of the sidecar's `wakes.ndjson`; every field is a closed value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KibitzerWakeOutcome {
    pub session_id: String,
    /// 1-based turn number across the whole sidecar lifetime, regardless of child generation.
    pub wake: u64,
    /// Which child answered: 1 for the first, +1 per backoff or reseed recreation.
    pub generation: u64,
    pub status: KibitzerWakeStatus,
    pub cause: Option<KibitzerWakeCause>,
    pub reason: Option<String>,
    /// The configuration state a start refusal resolved to; such failures are never diagnostic.
    pub configuration: Option<KibitzerWakeConfiguration>,
    pub model: Option<String>,
    /// Nudges the parent re-validated and handed to delivery.
    pub nudges: Vec<RecallNudge>,
    /// Candidate paths this wake put in front of the child (seed/follow-up plus every steer).
    pub candidate_count: usize,
    /// Steer envelopes the turn absorbed after it started.
    pub steered: usize,
    /// Child tool calls counted through the session subscription.
    pub tool_calls: usize,
    pub duration_ms: i64,
    /// Time the wake waited for its machine-wide lease before the turn could start.
    pub slot_wait_ms: i64,
    /// Parent cursor span of the events the wake carried.
    pub cursors: Option<KibitzerCursorSpan>,
    /// Context estimate of the child after this wake (provider usage, or char/4 when usage is absent).
    pub context_tokens: Option<i64>,
    /// Tokens the wake's assistant messages reported; absent when the provider reported none.
    pub usage: Option<KibitzerWakeUsage>,
    /// True only for a `failed` wake that is not a configuration state: budget, deadline, shutdown
    /// and category refusals never feed the failure streak.
    pub diagnostic: bool,
}

/// The reason a turn cancelled outside the sidecar is reported with.
const CHILD_TURN_CANCELLED_OUTSIDE: &str = "child turn was cancelled outside the sidecar";

/// `classifyWakeEnd`: a sidecar-initiated abort wins over whatever the engine reports for the
/// aborted turn (it settles as `cancelled`); otherwise the settled outcome is classified the way
/// the one-shot judge classified it, with the "empty response twice" rule keeping a silent child
/// out of the failure streak.
pub fn classify_wake_end(settle: &JudgeSettle, abort: Option<KibitzerWakeAbort>, accepted: &[RecallNudge]) -> KibitzerWakeEnd {
    match abort {
        Some(KibitzerWakeAbort::ToolBudget) => return KibitzerWakeEnd::ToolBudgetExceeded,
        Some(KibitzerWakeAbort::Deadline) => return KibitzerWakeEnd::Deadline,
        Some(KibitzerWakeAbort::Shutdown) => return KibitzerWakeEnd::Cancelled,
        None => {}
    }
    match classify_judge_turn(settle, accepted) {
        JudgeTurnClassification::Completed | JudgeTurnClassification::Empty => KibitzerWakeEnd::Completed,
        JudgeTurnClassification::Failed { cause, reason } => KibitzerWakeEnd::Failed {
            cause: match cause {
                JudgeFailureCause::ChildFailed => KibitzerWakeFailureCause::ChildFailed,
                JudgeFailureCause::ChildFailedUpstream => KibitzerWakeFailureCause::ChildFailedUpstream,
            },
            reason,
            configuration: None,
        },
        // The engine cancelled a turn the sidecar did not abort: the child is gone from under us.
        JudgeTurnClassification::Dropped { .. } => KibitzerWakeEnd::Failed {
            cause: KibitzerWakeFailureCause::ChildFailed,
            reason: Some(CHILD_TURN_CANCELLED_OUTSIDE.to_string()),
            configuration: None,
        },
    }
}

/// A child that never started: the same shape as a failed turn so the streak and backoff treat both
/// alike - unless the refusal is a configuration state, which never feeds the streak.
pub fn start_failure_end(message: &str, configuration: Option<KibitzerWakeConfiguration>) -> KibitzerWakeEnd {
    KibitzerWakeEnd::Failed {
        cause: KibitzerWakeFailureCause::StartFailed,
        reason: normalize_gate_reason(Some(message)),
        configuration,
    }
}

/// `isDiagnosticWakeEnd`: true only for a `failed` wake that is not a configuration state.
pub fn is_diagnostic_wake_end(end: &KibitzerWakeEnd) -> bool {
    matches!(end, KibitzerWakeEnd::Failed { configuration: None, .. })
}
