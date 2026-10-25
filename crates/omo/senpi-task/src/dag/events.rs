//! `dag/events.ts`: lane classification of journaled events.
//!
//! The TS payload builders are plain object literals; in Rust the [`DagRunEventPayload`] variants
//! are the builders, and their serde wire shape is what the builder tests pin.

use crate::dag::types::{DAG_RUN_EVENT_TYPES, DagEventLane, DagRunEventPayload, DagRunEventType};

/// Every journaled type is a boundary event; the activity lane is only for the separate
/// unsequenced activity transport.
pub fn dag_event_lane(event_type: DagRunEventType) -> DagEventLane {
    match event_type {
        DagRunEventType::RunCreated
        | DagRunEventType::RunStarted
        | DagRunEventType::RunPaused
        | DagRunEventType::RunResumed
        | DagRunEventType::RunCompleted
        | DagRunEventType::RunFailed
        | DagRunEventType::RunCancelled
        | DagRunEventType::WaveStarted
        | DagRunEventType::WaveCompleted
        | DagRunEventType::NodeTransitioned
        | DagRunEventType::NodeTaskAttached
        | DagRunEventType::NodeReused
        | DagRunEventType::NodeRetried
        | DagRunEventType::NodeSteered
        | DagRunEventType::DefinitionAmended
        | DagRunEventType::DiagnosticAdded
        | DagRunEventType::StreamOverflow
        | DagRunEventType::GuidanceAdded
        | DagRunEventType::GuidanceEdited
        | DagRunEventType::GuidanceRemoved
        | DagRunEventType::GuidanceDispatching
        | DagRunEventType::GuidanceDelivered
        | DagRunEventType::NodeCancelRequested
        | DagRunEventType::NodeRetryStaged
        | DagRunEventType::NodePauseRecovered
        | DagRunEventType::NodePauseLost => DagEventLane::Boundary,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown dag run event type: {0}")]
pub struct UnknownDagEventType(pub String);

/// Classify an untrusted wire type string (the TS exhaustiveness guard throws on unknown types).
pub fn dag_event_lane_of(event_type: &str) -> Result<DagEventLane, UnknownDagEventType> {
    DagRunEventType::parse(event_type)
        .map(dag_event_lane)
        .ok_or_else(|| UnknownDagEventType(event_type.to_string()))
}

impl DagRunEventType {
    pub const ALL: [Self; 26] = [
        Self::RunCreated,
        Self::RunStarted,
        Self::RunPaused,
        Self::RunResumed,
        Self::RunCompleted,
        Self::RunFailed,
        Self::RunCancelled,
        Self::WaveStarted,
        Self::WaveCompleted,
        Self::NodeTransitioned,
        Self::NodeTaskAttached,
        Self::NodeReused,
        Self::NodeRetried,
        Self::NodeSteered,
        Self::DefinitionAmended,
        Self::DiagnosticAdded,
        Self::StreamOverflow,
        Self::GuidanceAdded,
        Self::GuidanceEdited,
        Self::GuidanceRemoved,
        Self::GuidanceDispatching,
        Self::GuidanceDelivered,
        Self::NodeCancelRequested,
        Self::NodeRetryStaged,
        Self::NodePauseRecovered,
        Self::NodePauseLost,
    ];

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.as_str() == value)
    }
}

/// The lane of a payload, as the WAL writer stamps it.
pub fn payload_lane(payload: &DagRunEventPayload) -> DagEventLane {
    dag_event_lane(payload.event_type())
}

/// The 26 journaled type tags, in contract order.
pub fn journaled_event_types() -> [&'static str; 26] {
    DAG_RUN_EVENT_TYPES
}

#[cfg(test)]
#[path = "events_tests.rs"]
mod tests;
