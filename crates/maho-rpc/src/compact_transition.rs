//! senpi `modes/rpc/compact-transition.ts`: the typed transitions compact current state accepts
//! (senpi#4272, R3.C6.4) and the typed rejection `CompactSessionState::accept` returns when one fails
//! validation.
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompactBlockKind {
    Text,
    Thinking,
    ToolArguments,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompactPersistence {
    Pending,
    Appended,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockAddress {
    pub message_id: String,
    pub content_index: usize,
}

/// `block_start`: `first_ordinal` is the producer-owned ordinal of this message's first block_start,
/// stable across all its blocks. `covered_code_units` is the whole seed when later deltas replay it from
/// offset 0, or 0 when they continue after it; partial coverage is refused.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockStart {
    pub message_id: String,
    pub content_index: usize,
    pub epoch: String,
    pub ordinal: i64,
    pub first_ordinal: i64,
    pub kind: CompactBlockKind,
    pub seed: String,
    pub covered_code_units: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum CompactTransition {
    BlockStart(BlockStart),
    BlockDelta {
        message_id: String,
        content_index: usize,
        offset: usize,
        text: String,
    },
    BlockReplace {
        message_id: String,
        content_index: usize,
        text: String,
    },
    MessageStatus {
        message_id: String,
        persistence: CompactPersistence,
    },
    MessageRemove {
        message_id: String,
    },
}

impl CompactTransition {
    pub fn message_id(&self) -> &str {
        match self {
            Self::BlockStart(start) => &start.message_id,
            Self::BlockDelta { message_id, .. }
            | Self::BlockReplace { message_id, .. }
            | Self::MessageStatus { message_id, .. }
            | Self::MessageRemove { message_id } => message_id,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactRejection {
    InvalidStartIdentity,
    DuplicateOrStaleStart,
    UnknownBlock,
    DuplicateBlock,
    InvalidCoverage,
    NonContiguous,
    ConflictingOverlap,
    ColdOperationRequired,
    UnknownMessage,
}

impl CompactRejection {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InvalidStartIdentity => "invalid_start_identity",
            Self::DuplicateOrStaleStart => "duplicate_or_stale_start",
            Self::UnknownBlock => "unknown_block",
            Self::DuplicateBlock => "duplicate_block",
            Self::InvalidCoverage => "invalid_coverage",
            Self::NonContiguous => "non_contiguous",
            Self::ConflictingOverlap => "conflicting_overlap",
            Self::ColdOperationRequired => "cold_operation_required",
            Self::UnknownMessage => "unknown_message",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("compact transition rejected ({}): {detail}", .reason.as_str())]
pub struct CompactTransitionError {
    pub reason: CompactRejection,
    pub detail: String,
}

impl CompactTransitionError {
    pub fn new(reason: CompactRejection, detail: impl Into<String>) -> Self {
        Self {
            reason,
            detail: detail.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn transitions_keep_the_upstream_wire_shape() {
        let start: CompactTransition = serde_json::from_value(json!({
            "type":"block_start","messageId":"m","contentIndex":0,"epoch":"e","ordinal":3,
            "firstOrdinal":3,"kind":"tool_arguments","seed":"s","coveredCodeUnits":0
        }))
        .expect("block_start");
        assert_eq!(start.message_id(), "m");
        assert_eq!(
            serde_json::to_value(CompactTransition::MessageStatus {
                message_id: "m".into(),
                persistence: CompactPersistence::Appended
            })
            .expect("status"),
            json!({"type":"message_status","messageId":"m","persistence":"appended"})
        );
        assert_eq!(
            CompactTransitionError::new(CompactRejection::NonContiguous, "offset 2").to_string(),
            "compact transition rejected (non_contiguous): offset 2"
        );
    }
}
