//! Port of `tools/output/snapshot.ts`.

use chrono::DateTime;

use crate::state::TaskRecord;
use crate::tools::output::transcript::session_dir::child_session_dir;
use crate::tools::output::types::{LostBreadcrumbs, SuspendedDetails, TaskSnapshot};

pub const LOST_EXPLANATION: &str = "The task was marked lost: its process disappeared before a terminal result was recorded (crash, host restart, or an evicted resident child). Inspect the pid and session dir below; no result was captured.";

pub const SUSPENDED_EXPLANATION: &str = "suspended (resumes with session)";

const SUSPENDED_RESIDENCIES: [&str; 2] = ["persisted_only", "rpc_detached"];

/// Record snapshot for task_output status view (pi-task task-status result fields). For a `lost` task
/// it attaches read-only breadcrumbs (pid + the child's session dir) so the caller can investigate
/// without task_output ever reviving or touching child state.
pub fn build_task_snapshot(record: &TaskRecord, state_dir: &str, now: i64) -> TaskSnapshot {
    TaskSnapshot {
        task_id: record.task_id.clone(),
        name: record.name.clone(),
        description: record.description.clone(),
        task_summary: record.task_summary.clone(),
        status: record.status,
        residency_state: record.residency_state,
        suspended: is_suspended(record).then(|| SuspendedDetails {
            explanation: SUSPENDED_EXPLANATION.to_string(),
        }),
        execution_mode: record.execution_mode.clone(),
        model: record.model.clone(),
        parent_model: record.parent_model.clone(),
        parent_model_fallback: active_parent_model_fallback(record),
        effective_model: record.effective_model.clone(),
        resolved_model: record.resolved_model.clone(),
        agent_type: record.agent_type.clone(),
        category: record.category.clone(),
        parent_session_id: record.parent_session_id.clone(),
        root_session_id: record.root_session_id.clone(),
        age_ms: age_ms(record, now),
        pid: record.pid,
        child_session_id: record.child_session_id.clone(),
        final_response: record.final_response.clone(),
        error_message: record.error_message.clone(),
        run_stats: record.run_stats.clone(),
        lost: (record.status.as_str() == "lost").then(|| lost_breadcrumbs(record, state_dir)),
        stop: None,
    }
}

fn is_suspended(record: &TaskRecord) -> bool {
    SUSPENDED_RESIDENCIES.contains(&record.residency_state.as_str())
}

fn lost_breadcrumbs(record: &TaskRecord, state_dir: &str) -> LostBreadcrumbs {
    LostBreadcrumbs {
        explanation: LOST_EXPLANATION.to_string(),
        session_dir: child_session_dir(state_dir, &record.task_id),
        pid: record.pid,
    }
}

fn age_ms(record: &TaskRecord, now: i64) -> i64 {
    match DateTime::parse_from_rfc3339(&record.created_at) {
        Ok(created) => (now - created.timestamp_millis()).max(0),
        Err(_) => 0,
    }
}

/// Only expose the fallback while the current observed or selected route still uses the parent.
pub fn active_parent_model_fallback(
    record: &TaskRecord,
) -> Option<crate::completion::ParentModelFallback> {
    use crate::completion::ParentModelFallbackPhase;
    use crate::state::EffectiveModelSource;
    let fallback = record.parent_model_fallback.as_ref()?;
    use crate::completion::ParentModelFallbackReason as Reason;
    use crate::state::ParentModelFallbackReason as RecordedReason;
    let mut fallback = crate::completion::ParentModelFallback {
        from_model: fallback.from_model.clone(),
        reason: match fallback.reason {
            RecordedReason::UsageLimit => Reason::UsageLimit,
            RecordedReason::NotSignedIn => Reason::NotSignedIn,
            RecordedReason::Unavailable => Reason::Unavailable,
            RecordedReason::Refused => Reason::Refused,
            RecordedReason::Error => Reason::Error,
        },
        at: fallback.at.clone(),
        detail: fallback.detail.clone(),
        resets: fallback.resets.clone(),
        interrupted_tools: fallback.interrupted_tools.clone(),
        phase: None,
    };
    let parent = record.parent_model.as_deref();
    let same = |provider: &str, id: &str| parent == Some(format!("{provider}/{id}").as_str());
    if let Some(observed) = &record.effective_model {
        if same(&observed.provider, &observed.model_id) {
            return Some(fallback);
        }
        if observed.switched_by_operator == Some(true)
            || matches!(
                observed.source,
                EffectiveModelSource::Set
                    | EffectiveModelSource::Cycle
                    | EffectiveModelSource::Restore
                    | EffectiveModelSource::FallbackRevert
            )
        {
            return None;
        }
    }
    let selected = record.resolved_model.as_ref()?;
    if !same(&selected.provider, &selected.model_id) {
        return None;
    }
    fallback.phase = Some(
        if matches!(record.status.as_str(), "running" | "pending")
            || record.fallback_closing_child.is_some()
        {
            ParentModelFallbackPhase::Switching
        } else {
            ParentModelFallbackPhase::Unconfirmed
        },
    );
    Some(fallback)
}

#[cfg(test)]
mod parity_tests {
    use super::*;
    use crate::state::{
        EffectiveModelRecord, EffectiveModelSource, ParentModelFallback, ParentModelFallbackReason,
        TaskRecordInput, create_task_record,
    };
    #[test]
    fn snapshot_exposes_only_current_parent_fallback() {
        let mut record = create_task_record(TaskRecordInput::default(), Some(123)).expect("record");
        record.parent_model = Some("provider/parent".into());
        record.parent_model_fallback = Some(ParentModelFallback {
            phase: None,
            from_model: "provider/exhausted".into(),
            reason: ParentModelFallbackReason::UsageLimit,
            at: "now".into(),
            detail: None,
            resets: Some("tomorrow".into()),
            interrupted_tools: Some(vec!["bash".into()]),
        });
        let observed: EffectiveModelRecord = serde_json::from_value(serde_json::json!({"provider":"provider", "model_id":"parent", "display":"provider/parent", "source":"fallback"})).expect("effective");
        record.effective_model = Some(observed);
        let snapshot = build_task_snapshot(&record, "/tmp", 0);
        assert!(snapshot.parent_model_fallback.is_some());
        assert_eq!(snapshot.parent_model.as_deref(), Some("provider/parent"));
        record.effective_model.as_mut().expect("effective").source = EffectiveModelSource::Set;
        record.effective_model.as_mut().expect("effective").model_id = "elsewhere".into();
        assert!(
            build_task_snapshot(&record, "/tmp", 0)
                .parent_model_fallback
                .is_none()
        );
    }
}
