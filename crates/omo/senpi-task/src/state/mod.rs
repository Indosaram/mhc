//! Task record model and lifecycle state machine (`state/` in TypeScript).

mod id;
mod isolation;
mod record;
mod run_fence;
pub mod recovery_presentation;
mod transitions;
mod types;

pub(crate) use id::system_now_ms;
pub use id::{
    InvalidTaskIdError, TaskId, TaskIdFactory, TaskIdSpaceExhaustedError, bump_task_id,
    create_task_id, create_task_id_factory, parse_task_id, sync_task_id_floor,
};
pub use isolation::{
    IsolationMergeKind, IsolationMergeMode, IsolationMergeResult, IsolationRecord,
    TaskIsolationSpec,
};
pub use record::create_task_record;
pub use run_fence::{LEGACY_RUN_START_EPOCH, current_run_start_epoch};
pub use transitions::{
    LostReason, mark_record_lost_for_reconciliation, messageability, messageability_with_runner,
    read_resolved_reasoning, resolved_reasoning_fields, transition_task_record,
};
pub use types::{
    ParentModelFallback, ParentModelFallbackReason, FallbackClosingChild, StartQueued,
    ReviveDeliveryUncertainty, RecordedRunPause, RunPauseRef, RunPauseKind,
    COST_REPORT_STATUSES, LiveRunStats, PARENT_MODEL_FALLBACK_REASONS, ParentModelFallbackPhase,
    AwaitingInputReason, BackgroundMode, CancelRequest, CostReportStatus, DeliverAs,
    DurationSourceStatus, EffectiveModelRecord, EffectiveModelSource, HostSessionIdentity,
    Messageability, PendingSteeringEntry, PendingSteeringWorkpoolMetadata, RESIDENCY_STATES,
    RESOLVED_MODEL_SOURCES, ResidencyState, ResolvedModelRecord, ResolvedModelSource, RunnerKind,
    SpawnSpecV1, SuspensionReason, TASK_STATUSES, TaskAwaitingInput, TaskNotification, TaskRecord,
    TaskRecordInput, TaskRunStats, TaskSpawnSpec, TaskStatus, TaskTransition, TaskTransitionAudit,
    TaskTransitionResult, TokenCoverageStatus, is_spawn_spec_v1,
};

#[cfg(test)]
#[path = "state_tests.rs"]
mod tests;
