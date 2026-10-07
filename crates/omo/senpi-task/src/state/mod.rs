//! Task record model and lifecycle state machine (`state/` in TypeScript).

mod id;
mod isolation;
mod record;
mod transitions;
mod types;

pub(crate) use id::system_now_ms;
pub use id::{
    InvalidTaskIdError, TaskId, TaskIdFactory, TaskIdSpaceExhaustedError, bump_task_id,
    create_task_id, create_task_id_factory, parse_task_id, sync_task_id_floor,
};
pub use record::create_task_record;
pub use isolation::{
    IsolationMergeKind, IsolationMergeMode, IsolationMergeResult, IsolationRecord,
    TaskIsolationSpec,
};
pub use transitions::{
    LostReason, mark_record_lost_for_reconciliation, messageability, read_resolved_reasoning,
    resolved_reasoning_fields, transition_task_record,
};
pub use types::{
    DeliverAs, HostSessionIdentity, Messageability, PendingSteeringEntry, RESIDENCY_STATES,
    RESOLVED_MODEL_SOURCES, ResidencyState, ResolvedModelRecord, ResolvedModelSource, RunnerKind,
    SpawnSpecV1, TASK_STATUSES, TaskNotification, TaskRecord, TaskRecordInput, TaskRunStats,
    TaskSpawnSpec, TaskStatus, TaskTransition, TaskTransitionAudit, TaskTransitionResult,
    is_spawn_spec_v1,
};

#[cfg(test)]
#[path = "state_tests.rs"]
mod tests;
