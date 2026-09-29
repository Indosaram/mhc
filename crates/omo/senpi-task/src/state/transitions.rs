use super::types::{
    Messageability, ResidencyState, ResolvedModelRecord, TaskRecord, TaskStatus, TaskTransition,
    TaskTransitionAudit, TaskTransitionResult,
};

/// Suspended residencies are never continuable: messaging a suspended child must not wake it.
pub fn messageability(status: TaskStatus, residency_state: ResidencyState) -> Messageability {
    match residency_state {
        ResidencyState::Disposed | ResidencyState::PersistedOnly | ResidencyState::RpcDetached => {
            return Messageability::NotContinuable;
        }
        ResidencyState::Resident | ResidencyState::Evicted => {}
    }
    let resident = residency_state == ResidencyState::Resident;
    match status {
        TaskStatus::Pending | TaskStatus::Running if resident => Messageability::Steer,
        TaskStatus::Completed | TaskStatus::Error | TaskStatus::Interrupted if resident => {
            Messageability::Revive
        }
        TaskStatus::Pending
        | TaskStatus::Running
        | TaskStatus::Completed
        | TaskStatus::Error
        | TaskStatus::Interrupted
        | TaskStatus::Cancelled
        | TaskStatus::Lost => Messageability::NotContinuable,
    }
}

/// Legacy spellings stay in the read chain until the deprecation window closes.
pub fn read_resolved_reasoning(model: &ResolvedModelRecord) -> Option<&str> {
    model
        .reasoning
        .as_deref()
        .or(model.reasoning_effort.as_deref())
        .or(model.variant.as_deref())
}

/// `{ reasoning, variant }` both carrying the effective reasoning, or neither.
pub fn resolved_reasoning_fields(model: &ResolvedModelRecord) -> (Option<String>, Option<String>) {
    let reasoning = read_resolved_reasoning(model).map(str::to_string);
    (reasoning.clone(), reasoning)
}

fn transition_status(transition: &TaskTransition, current: TaskStatus) -> TaskStatus {
    match transition {
        TaskTransition::Start { .. } => TaskStatus::Running,
        TaskTransition::Complete { .. } => TaskStatus::Completed,
        TaskTransition::Fail { .. } => TaskStatus::Error,
        TaskTransition::Cancel { .. } => TaskStatus::Cancelled,
        TaskTransition::Interrupt { .. } => TaskStatus::Interrupted,
        TaskTransition::Lose { .. } => TaskStatus::Lost,
        TaskTransition::Evict { .. }
        | TaskTransition::Dispose { .. }
        | TaskTransition::PersistOnly { .. }
        | TaskTransition::DetachRpc { .. }
        | TaskTransition::MarkResident { .. } => current,
    }
}

fn transition_residency(transition: &TaskTransition, current: ResidencyState) -> ResidencyState {
    match transition {
        TaskTransition::Evict { .. } => ResidencyState::Evicted,
        TaskTransition::Dispose { .. } => ResidencyState::Disposed,
        TaskTransition::PersistOnly { .. } => ResidencyState::PersistedOnly,
        TaskTransition::DetachRpc { .. } => ResidencyState::RpcDetached,
        TaskTransition::MarkResident { .. } => ResidencyState::Resident,
        TaskTransition::Start { .. }
        | TaskTransition::Complete { .. }
        | TaskTransition::Fail { .. }
        | TaskTransition::Cancel { .. }
        | TaskTransition::Interrupt { .. }
        | TaskTransition::Lose { .. } => current,
    }
}

fn changes_only_residency(transition: &TaskTransition) -> bool {
    match transition {
        TaskTransition::Evict { .. }
        | TaskTransition::Dispose { .. }
        | TaskTransition::PersistOnly { .. }
        | TaskTransition::DetachRpc { .. }
        | TaskTransition::MarkResident { .. } => true,
        TaskTransition::Start { .. }
        | TaskTransition::Complete { .. }
        | TaskTransition::Fail { .. }
        | TaskTransition::Cancel { .. }
        | TaskTransition::Interrupt { .. }
        | TaskTransition::Lose { .. } => false,
    }
}

fn is_status_transition_allowed(current: TaskStatus, transition: &TaskTransition) -> bool {
    match transition {
        TaskTransition::Start { .. } => current == TaskStatus::Pending,
        TaskTransition::Cancel { .. } => {
            current == TaskStatus::Running || current == TaskStatus::Pending
        }
        TaskTransition::Complete { .. }
        | TaskTransition::Fail { .. }
        | TaskTransition::Interrupt { .. } => current == TaskStatus::Running,
        TaskTransition::Lose { .. } => false,
        TaskTransition::Evict { .. }
        | TaskTransition::Dispose { .. }
        | TaskTransition::PersistOnly { .. }
        | TaskTransition::DetachRpc { .. }
        | TaskTransition::MarkResident { .. } => true,
    }
}

fn apply_transition_fields(record: &TaskRecord, transition: &TaskTransition) -> TaskRecord {
    let mut next = record.clone();
    match transition {
        TaskTransition::Start {
            pid,
            child_session_id,
            ..
        } => {
            if pid.is_some() {
                next.pid = *pid;
            }
            if child_session_id.is_some() {
                next.child_session_id.clone_from(child_session_id);
            }
        }
        TaskTransition::Complete {
            final_response,
            run_stats,
            ..
        } => {
            next.final_response = Some(final_response.clone());
            if run_stats.is_some() {
                next.run_stats.clone_from(run_stats);
            }
        }
        TaskTransition::Fail {
            error_message,
            killed,
            run_stats,
            ..
        } => {
            next.error_message = Some(error_message.clone());
            if *killed {
                next.killed = Some(true);
            }
            if run_stats.is_some() {
                next.run_stats.clone_from(run_stats);
            }
        }
        TaskTransition::Lose { error_message, .. } => {
            next.error_message = Some(error_message.clone());
        }
        TaskTransition::Cancel {
            error_message,
            run_stats,
            ..
        }
        | TaskTransition::Interrupt {
            error_message,
            run_stats,
            ..
        } => {
            if error_message.is_some() {
                next.error_message.clone_from(error_message);
            }
            if run_stats.is_some() {
                next.run_stats.clone_from(run_stats);
            }
        }
        TaskTransition::PersistOnly { .. } => {
            // In-process suspension: the owning engine is gone, so both pids are meaningless.
            next.host_pid = None;
            next.pid = None;
        }
        TaskTransition::DetachRpc { .. } => {
            // RPC suspension keeps the last pid so reconcile can still reap the orphaned process.
            next.host_pid = None;
        }
        TaskTransition::Evict { .. }
        | TaskTransition::Dispose { .. }
        | TaskTransition::MarkResident { .. } => {}
    }
    next
}

pub fn transition_task_record(
    record: &TaskRecord,
    transition: &TaskTransition,
) -> TaskTransitionResult {
    let next_status = transition_status(transition, record.status);
    if record.status.is_terminal() && !changes_only_residency(transition) {
        return TaskTransitionResult {
            applied: false,
            record: record.clone(),
            audit: TaskTransitionAudit::LateTransitionIgnored {
                attempted_status: next_status,
                current_status: record.status,
            },
        };
    }
    if !is_status_transition_allowed(record.status, transition) {
        return TaskTransitionResult {
            applied: false,
            record: record.clone(),
            audit: TaskTransitionAudit::InvalidTransitionIgnored {
                attempted_status: next_status,
                current_status: record.status,
            },
        };
    }
    let mut next = apply_transition_fields(record, transition);
    next.status = next_status;
    next.residency_state = transition_residency(transition, record.residency_state);
    next.updated_at = transition.timestamp().to_string();
    let audit = TaskTransitionAudit::TransitionApplied {
        status: next.status,
        residency_state: next.residency_state,
    };
    TaskTransitionResult {
        applied: true,
        record: next,
        audit,
    }
}

/// How reconciliation treats a record that is already `lost`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LostReason {
    KeepExisting,
    Update,
}

pub fn mark_record_lost_for_reconciliation(
    record: &TaskRecord,
    timestamp: &str,
    error_message: &str,
    reason: LostReason,
) -> TaskTransitionResult {
    let ignored = TaskTransitionResult {
        applied: false,
        record: record.clone(),
        audit: TaskTransitionAudit::LateTransitionIgnored {
            attempted_status: TaskStatus::Lost,
            current_status: record.status,
        },
    };
    if record.status.is_terminal() && record.status != TaskStatus::Lost {
        return ignored;
    }
    if record.status == TaskStatus::Lost && reason == LostReason::KeepExisting {
        return ignored;
    }
    let mut next = record.clone();
    next.status = TaskStatus::Lost;
    next.error_message = Some(error_message.to_string());
    next.updated_at = timestamp.to_string();
    let audit = TaskTransitionAudit::TransitionApplied {
        status: next.status,
        residency_state: next.residency_state,
    };
    TaskTransitionResult {
        applied: true,
        record: next,
        audit,
    }
}
