//! Port of senpi `packages/coding-agent/src/core/pending-input-types.ts`.
//!
//! Closed types of the one pending-input lifecycle (senpi#3549). A pending input is admitted into
//! durable custody and ends delivered, removed, or failed; `edited` and `superseded` are facts of the
//! same record, never a second runnable input. Only `reduce_pending_input` changes a `PendingInput`;
//! every surface renders `project_pending_input`. The serde shapes are the journal's JSON.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PendingInputPhase {
    Queued,
    Sending,
    Delivered,
    Removed,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PendingInputMode {
    #[serde(rename = "steer")]
    Steer,
    #[serde(rename = "followUp")]
    FollowUp,
}

/// Whether a failed input can be resent: `not_delivered` is proven safe, `uncertain` is not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryCertainty {
    NotDelivered,
    Uncertain,
}

/// Why an input was retired by removal; an explicit user or owner action every time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RemovalCause {
    Remove,
    Stop,
    ChildCancel,
}

/// How delivery was proven: a committed user entry with this identity, or a durable handled-command receipt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryWitness {
    Consumed,
    Handled,
}

/// The current, lifetime-scoped handle of an input. A reopen issues a new one; revision restarts at 1.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingInputHandle {
    pub lifetime_id: String,
    pub queued_id: String,
}

/// The reservation a consumer holds while it hands one input off; a stale token cannot change state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeliveryToken {
    pub owner_id: String,
    pub writer_generation: u64,
    pub lifetime_id: String,
    pub attempt_id: String,
    pub client_message_id: String,
}

/// A retired content version: an acknowledged edit replaced it, and it is never runnable again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SupersededVersion {
    /// The lifetime that revision belongs to; revisions restart at 1 on reopen.
    pub lifetime_id: String,
    pub revision: u64,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingInputFailure {
    pub delivery: DeliveryCertainty,
    pub reason: String,
    /// The reservation that failed, kept so later evidence must name the attempt it is about.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt: Option<DeliveryToken>,
}

/// The recorded answer of one operation, replayed byte for byte when the same effect id repeats.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordedOperation {
    pub fingerprint: String,
    pub result: PendingInputOperationResult,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingInput {
    /// Durable key with the session: survives reopen; never a queue id, text, or order.
    pub client_message_id: String,
    /// Fingerprint of the original submission; a reused id with another payload is refused.
    pub fingerprint: String,
    pub handle: PendingInputHandle,
    /// 1 at admission, +1 only after a successful edit.
    pub revision: u64,
    /// Survives reopen, unlike the revision.
    pub has_edits: bool,
    pub text: String,
    pub attachments: Vec<String>,
    pub requested_mode: PendingInputMode,
    pub phase: PendingInputPhase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<PendingInputFailure>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub removed_by: Option<RemovalCause>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivered_by: Option<DeliveryWitness>,
    /// The live reservation while `sending`; absent in every other phase.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt: Option<DeliveryToken>,
    pub attempt_count: u64,
    pub superseded: Vec<SupersededVersion>,
    pub operations: BTreeMap<String, RecordedOperation>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubmitEvent {
    pub client_message_id: String,
    pub fingerprint: String,
    pub handle: PendingInputHandle,
    pub text: String,
    pub attachments: Vec<String>,
    pub requested_mode: PendingInputMode,
}

/// A mutation names the handle and revision it was based on, and carries an effect id for replay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MutationTarget {
    pub effect_id: String,
    pub handle: PendingInputHandle,
    pub expected_revision: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelCause {
    Stop,
    ChildCancel,
}

impl From<CancelCause> for RemovalCause {
    fn from(cause: CancelCause) -> Self {
        match cause {
            CancelCause::Stop => Self::Stop,
            CancelCause::ChildCancel => Self::ChildCancel,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PendingInputEvent {
    Submit(SubmitEvent),
    RejectBeforeDispatch {
        reason: String,
    },
    Edit {
        target: MutationTarget,
        text: String,
        attachments: Vec<String>,
    },
    Remove {
        target: MutationTarget,
    },
    Retry {
        effect_id: String,
        handle: PendingInputHandle,
    },
    Reserve {
        token: DeliveryToken,
    },
    /// The consumer gave the attempt back before any of it was committed or sent: queued again, same id.
    Release {
        token: DeliveryToken,
    },
    Consumed {
        token: DeliveryToken,
    },
    /// A handled-command receipt with the reservation's token.
    HandledToken {
        token: DeliveryToken,
    },
    /// A handled-command receipt naming the lifetime of a still-queued input.
    HandledLifetime {
        lifetime_id: String,
    },
    Refused {
        token: DeliveryToken,
        reason: String,
    },
    Ambiguous {
        token: DeliveryToken,
        reason: String,
    },
    /// An identity-bearing delivery witness found after the fact, for one failed attempt.
    Witnessed {
        attempt_id: String,
    },
    /// Complete, fenced evidence that no consumer can still deliver the named failed attempt.
    NonDeliveryProven {
        attempt_id: String,
    },
    /// Teardown that is not an explicit user action: reload, unload, crash, handoff.
    Teardown,
    OwnerLost {
        token: DeliveryToken,
    },
    /// An explicit Stop or child cancellation; `consumer_fenced` proves an in-flight attempt was not consumed.
    Cancel {
        cause: CancelCause,
        consumer_fenced: bool,
    },
    /// A reopen. `attempts_unsent` is the journal owner's proof that an attempt still `sending` was never
    /// handed to its consumer; without it the attempt stays uncertain.
    Rebind {
        handle: PendingInputHandle,
        attempts_unsent: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationOutcome {
    Admitted,
    Replayed,
    ClientMessageIdConflict,
    Edited,
    Removed,
    Requeued,
    AlreadyDelivered,
    NotFound,
    Conflict,
    Sending,
    NotRetryable,
    NotEditable,
    Unresolved,
    EffectIdConflict,
    StaleHandle,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingInputOperationResult {
    pub outcome: OperationOutcome,
    pub client_message_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_revision: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// The lifetime this answer was recorded in; a replay names its original lifetime.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifetime_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replayed: Option<bool>,
}

/// What the store must do after committing `next`; the reducer itself never performs I/O.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PendingInputEffect {
    Persist,
    Dispatch { token: DeliveryToken },
    RestoreToDraft { text: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingInputDecision {
    pub next: Option<PendingInput>,
    pub result: Option<PendingInputOperationResult>,
    pub effects: Vec<PendingInputEffect>,
}
