//! senpi `modes/rpc/compact-recovery-types.ts`: typed tokens, leases, failures and counters of the
//! recovery protocol (R3.C1.2-C1.5, S13, S14).

use maho_core::session_transcript_journal::{PersistedBaseError, TranscriptUnavailableReason};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// How a peer reaches a routing session: its own attachment, or (S16) a mirror of another destination's stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryRouteKind {
    Ordinary,
    Mirror,
}

impl RecoveryRouteKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ordinary => "ordinary",
            Self::Mirror => "mirror",
        }
    }
}

/// One admission of a peer to a routing session (R3.C4, RULING-46 item 1).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryAdmission {
    pub socket_epoch: String,
    pub admission_id: String,
    pub route_kind: RecoveryRouteKind,
    pub peer: String,
    pub session_id: String,
    pub session_epoch: String,
    pub generation: u64,
}

impl RecoveryAdmission {
    pub fn caller(&self) -> RecoveryCaller {
        RecoveryCaller {
            socket_epoch: self.socket_epoch.clone(),
            session_id: self.session_id.clone(),
        }
    }
}

/// Who asks for a token: the peer incarnation and the routing session the lookup is for.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryCaller {
    pub socket_epoch: String,
    pub session_id: String,
}

/// The identity of one frozen cut C (R3.C1.2): what the accepted owners pinned, never a recovery counter.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryCutIdentity {
    pub session_epoch: String,
    pub sequence: u64,
    pub history_version: u64,
    pub leaf_id: Option<String>,
    pub active_message_id: Option<String>,
    pub turn_id: Option<String>,
}

/// A checkpoint token (R3.C1.5), tagged so S13 can add an event-lease variant without changing S08's callers.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckpointToken {
    pub kind: String,
    pub id: String,
    pub admission: RecoveryAdmission,
    pub cut: RecoveryCutIdentity,
}

impl CheckpointToken {
    pub fn new(id: String, admission: RecoveryAdmission, cut: RecoveryCutIdentity) -> Self {
        Self {
            kind: "checkpoint".into(),
            id,
            admission,
            cut,
        }
    }
}

pub type RecoveryToken = CheckpointToken;

/// An ordinary-event lease (S13, RULING-65 S13-Q1 A): the accepted record at `sequence` of `sessionEpoch`,
/// pinned for one peer admission and paged as `componentRef`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EventLease {
    pub kind: String,
    pub id: String,
    pub admission_id: String,
    pub session_epoch: String,
    pub sequence: u64,
    pub component_ref: String,
    pub length: usize,
    pub digest: String,
}

impl EventLease {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: String,
        admission_id: String,
        session_epoch: String,
        sequence: u64,
        component_ref: String,
        length: usize,
        digest: String,
    ) -> Self {
        Self {
            kind: "event".into(),
            id,
            admission_id,
            session_epoch,
            sequence,
            component_ref,
            length,
            digest,
        }
    }
}

/// Every lease the recovery owner hands out, by `kind`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RecoveryLease {
    Checkpoint(CheckpointToken),
    Event(EventLease),
}

/// Why a token needs repair. A token minted for another peer or session is `unknown_token` (RULING-56 (1)).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RepairReason {
    UnknownToken,
    Released,
    LaneRetired,
    StaleEpoch,
    PeerGone,
    SessionClosed,
}

impl RepairReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::UnknownToken => "unknown_token",
            Self::Released => "released",
            Self::LaneRetired => "lane_retired",
            Self::StaleEpoch => "stale_epoch",
            Self::PeerGone => "peer_gone",
            Self::SessionClosed => "session_closed",
        }
    }
}

/// Why the host could not serve a pinned cut: a closed union (RULING-58 (2)), never a free string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StorageUnavailableReason {
    CompactStateUnavailable,
    TransitionOverWorkerFrameLimit,
    TranscriptFileIdentityUnavailable,
    TranscriptTailTrimmed,
    TranscriptTailOverByteLimit,
    TranscriptAncestryOverByteLimit,
    TranscriptEntryUnidentified,
    BaseFileMissing,
    BaseFileReplaced,
    BaseFileTruncated,
    BaseFingerprintMismatch,
    Retired,
    Range,
    SizeLimit,
    CountLimit,
    Ownership,
    Unavailable,
    Integrity,
    Aborted,
}

impl StorageUnavailableReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CompactStateUnavailable => "compact_state_unavailable",
            Self::TransitionOverWorkerFrameLimit => "transition_over_worker_frame_limit",
            Self::TranscriptFileIdentityUnavailable => "transcript_file_identity_unavailable",
            Self::TranscriptTailTrimmed => "transcript_tail_trimmed",
            Self::TranscriptTailOverByteLimit => "transcript_tail_over_byte_limit",
            Self::TranscriptAncestryOverByteLimit => "transcript_ancestry_over_byte_limit",
            Self::TranscriptEntryUnidentified => "transcript_entry_unidentified",
            Self::BaseFileMissing => "base_file_missing",
            Self::BaseFileReplaced => "base_file_replaced",
            Self::BaseFileTruncated => "base_file_truncated",
            Self::BaseFingerprintMismatch => "base_fingerprint_mismatch",
            Self::Retired => "retired",
            Self::Range => "range",
            Self::SizeLimit => "size_limit",
            Self::CountLimit => "count_limit",
            Self::Ownership => "ownership",
            Self::Unavailable => "unavailable",
            Self::Integrity => "integrity",
            Self::Aborted => "aborted",
        }
    }
}

impl From<TranscriptUnavailableReason> for StorageUnavailableReason {
    fn from(reason: TranscriptUnavailableReason) -> Self {
        match reason {
            TranscriptUnavailableReason::TransitionOverWorkerFrameLimit => {
                Self::TransitionOverWorkerFrameLimit
            }
            TranscriptUnavailableReason::TranscriptFileIdentityUnavailable => {
                Self::TranscriptFileIdentityUnavailable
            }
            TranscriptUnavailableReason::TranscriptTailTrimmed => Self::TranscriptTailTrimmed,
            TranscriptUnavailableReason::TranscriptTailOverByteLimit => {
                Self::TranscriptTailOverByteLimit
            }
            TranscriptUnavailableReason::TranscriptAncestryOverByteLimit => {
                Self::TranscriptAncestryOverByteLimit
            }
            TranscriptUnavailableReason::TranscriptEntryUnidentified => {
                Self::TranscriptEntryUnidentified
            }
        }
    }
}

impl From<PersistedBaseError> for StorageUnavailableReason {
    fn from(err: PersistedBaseError) -> Self {
        match err {
            PersistedBaseError::BaseFileMissing => Self::BaseFileMissing,
            PersistedBaseError::BaseFileReplaced => Self::BaseFileReplaced,
            PersistedBaseError::BaseFileTruncated => Self::BaseFileTruncated,
            PersistedBaseError::BaseFingerprintMismatch => Self::BaseFingerprintMismatch,
        }
    }
}

/// The typed failure of every token operation (S14-Q2 shape).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RecoveryFailure {
    #[serde(rename_all = "camelCase")]
    RepairNeeded { reason: RepairReason },
    #[serde(rename = "repair_needed", rename_all = "camelCase")]
    SupersededToken {
        reason: String,
        newer_generation: u64,
    },
    #[serde(rename_all = "camelCase")]
    StorageUnavailable { reason: StorageUnavailableReason },
}

impl<'de> Deserialize<'de> for RecoveryFailure {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct RecoveryFailureWire {
            kind: String,
            reason: serde_json::Value,
            newer_generation: Option<u64>,
        }
        let wire = RecoveryFailureWire::deserialize(deserializer)?;
        match wire.kind.as_str() {
            "repair_needed" => {
                if wire.reason.as_str() == Some("superseded_token") {
                    let newer_generation = wire
                        .newer_generation
                        .ok_or_else(|| serde::de::Error::missing_field("newerGeneration"))?;
                    Ok(Self::SupersededToken {
                        reason: "superseded_token".to_owned(),
                        newer_generation,
                    })
                } else {
                    let reason =
                        RepairReason::deserialize(wire.reason).map_err(serde::de::Error::custom)?;
                    Ok(Self::RepairNeeded { reason })
                }
            }
            "storage_unavailable" => {
                let reason = StorageUnavailableReason::deserialize(wire.reason)
                    .map_err(serde::de::Error::custom)?;
                Ok(Self::StorageUnavailable { reason })
            }
            other => Err(serde::de::Error::unknown_variant(
                other,
                &["repair_needed", "storage_unavailable"],
            )),
        }
    }
}

impl RecoveryFailure {
    pub const fn kind_str(&self) -> &'static str {
        match self {
            Self::RepairNeeded { .. } | Self::SupersededToken { .. } => "repair_needed",
            Self::StorageUnavailable { .. } => "storage_unavailable",
        }
    }

    pub const fn reason_str(&self) -> &'static str {
        match self {
            Self::RepairNeeded { reason } => reason.as_str(),
            Self::SupersededToken { .. } => "superseded_token",
            Self::StorageUnavailable { reason } => reason.as_str(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefusalReason {
    UnknownToken,
    Released,
    LaneRetired,
    StaleEpoch,
    PeerGone,
    SessionClosed,
    SupersededToken,
    StorageUnavailable,
}

impl RefusalReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::UnknownToken => "unknown_token",
            Self::Released => "released",
            Self::LaneRetired => "lane_retired",
            Self::StaleEpoch => "stale_epoch",
            Self::PeerGone => "peer_gone",
            Self::SessionClosed => "session_closed",
            Self::SupersededToken => "superseded_token",
            Self::StorageUnavailable => "storage_unavailable",
        }
    }
}

impl From<RepairReason> for RefusalReason {
    fn from(reason: RepairReason) -> Self {
        match reason {
            RepairReason::UnknownToken => Self::UnknownToken,
            RepairReason::Released => Self::Released,
            RepairReason::LaneRetired => Self::LaneRetired,
            RepairReason::StaleEpoch => Self::StaleEpoch,
            RepairReason::PeerGone => Self::PeerGone,
            RepairReason::SessionClosed => Self::SessionClosed,
        }
    }
}

pub fn refusal_reason(failure: &RecoveryFailure) -> RefusalReason {
    match failure {
        RecoveryFailure::StorageUnavailable { .. } => RefusalReason::StorageUnavailable,
        RecoveryFailure::SupersededToken { .. } => RefusalReason::SupersededToken,
        RecoveryFailure::RepairNeeded { reason } => (*reason).into(),
    }
}

pub type RecoveryOutcome<T> = Result<T, RecoveryFailure>;

/// One peer + session stream as a holder may read it (RULING-42 Q2 A).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryStreamState {
    pub session_epoch: String,
    pub generation: u64,
    pub routes: Vec<String>,
}

/// The owner's counters.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryCounters {
    pub open: usize,
    pub streams: usize,
    pub frozen: u64,
    pub reclaimed: u64,
    pub refused: BTreeMap<RefusalReason, u64>,
    pub unavailable: BTreeMap<StorageUnavailableReason, u64>,
}

pub fn recovered<T>(value: T) -> RecoveryOutcome<T> {
    Ok(value)
}

pub fn repair_needed(reason: RepairReason) -> RecoveryFailure {
    RecoveryFailure::RepairNeeded { reason }
}

pub fn superseded_token(newer_generation: u64) -> RecoveryFailure {
    RecoveryFailure::SupersededToken {
        reason: "superseded_token".into(),
        newer_generation,
    }
}

pub fn storage_unavailable(reason: StorageUnavailableReason) -> RecoveryFailure {
    RecoveryFailure::StorageUnavailable { reason }
}

/// Returned/thrown inside a token's operation lease when the cut stopped being valid.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("recovery refused ({}: {})", .failure.kind_str(), .failure.reason_str())]
pub struct RecoveryRefusedError {
    pub failure: RecoveryFailure,
}

impl RecoveryRefusedError {
    pub const fn new(failure: RecoveryFailure) -> Self {
        Self { failure }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_failures_to_refusal_reasons_and_serializes_wire_shapes() {
        let sup = superseded_token(4);
        assert_eq!(refusal_reason(&sup), RefusalReason::SupersededToken);
        let json = serde_json::to_value(&sup).expect("serialize");
        assert_eq!(json["kind"], "repair_needed");
        assert_eq!(json["reason"], "superseded_token");
        assert_eq!(json["newerGeneration"], 4);

        let unavail = storage_unavailable(StorageUnavailableReason::BaseFingerprintMismatch);
        assert_eq!(refusal_reason(&unavail), RefusalReason::StorageUnavailable);
        assert_eq!(unavail.reason_str(), "base_fingerprint_mismatch");

        let err = RecoveryRefusedError::new(repair_needed(RepairReason::PeerGone));
        assert_eq!(
            err.to_string(),
            "recovery refused (repair_needed: peer_gone)"
        );
    }
}
