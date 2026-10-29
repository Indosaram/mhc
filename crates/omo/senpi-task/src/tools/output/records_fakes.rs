//! Test fixture: fully-formed `TaskRecord` builder for tool output tests
//! (port of `tools/output/__fixtures__/records.ts`).

use crate::state::{ResidencyState, ResolvedModelRecord, TaskRecord, TaskStatus};
use crate::test_support::base_record;

#[derive(Debug, Clone, Default)]
pub(crate) struct RecordOverrides {
    pub(crate) task_id: Option<String>,
    pub(crate) name: Option<String>,
    pub(crate) description: Option<String>,
    pub(crate) status: Option<TaskStatus>,
    pub(crate) parent_session_id: Option<String>,
    pub(crate) execution_mode: Option<String>,
    pub(crate) model: Option<String>,
    pub(crate) agent_type: Option<String>,
    pub(crate) category: Option<String>,
    pub(crate) resolved_model: Option<ResolvedModelRecord>,
    pub(crate) pid: Option<i64>,
    pub(crate) child_session_id: Option<String>,
    pub(crate) created_at: Option<String>,
    pub(crate) updated_at: Option<String>,
    pub(crate) final_response: Option<String>,
    pub(crate) error_message: Option<String>,
    pub(crate) run_epoch: Option<i64>,
    pub(crate) notified_epoch: Option<i64>,
    pub(crate) residency_state: Option<ResidencyState>,
}

/// A fully-formed TaskRecord for tool tests: every required field defaulted, terminal fields opt-in.
pub(crate) fn make_record(overrides: RecordOverrides) -> TaskRecord {
    let task_id = overrides
        .task_id
        .unwrap_or_else(|| "st_0000000000000000".to_string());
    let parent_session_id = overrides
        .parent_session_id
        .unwrap_or_else(|| "session-parent".to_string());
    let timestamp = overrides
        .updated_at
        .unwrap_or_else(|| "2024-12-03T14:00:00.000Z".to_string());

    let mut record = base_record(&task_id, &parent_session_id);
    record.description = overrides.description;
    record.root_session_id = "session-root".to_string();
    record.depth = 0;
    record.status = overrides.status.unwrap_or(TaskStatus::Running);
    record.residency_state = overrides
        .residency_state
        .unwrap_or(ResidencyState::Resident);
    record.execution_mode = overrides
        .execution_mode
        .unwrap_or_else(|| "in-process".to_string());
    record.model = overrides
        .model
        .unwrap_or_else(|| "claude-sonnet-4-5".to_string());
    record.created_at = overrides.created_at.unwrap_or_else(|| timestamp.clone());
    record.updated_at = timestamp;
    record.notify_on_terminal = false;
    record.notification.run_epoch = overrides.run_epoch.unwrap_or(0);
    record.notification.notified_epoch = overrides.notified_epoch.unwrap_or(-1);
    record.name = overrides.name;
    record.resolved_model = overrides.resolved_model;
    record.agent_type = overrides.agent_type;
    record.category = overrides.category;
    record.pid = overrides.pid;
    record.child_session_id = overrides.child_session_id;
    record.final_response = overrides.final_response;
    record.error_message = overrides.error_message;
    record
}

#[test]
fn make_record_defaults_every_required_field() {
    use pretty_assertions::assert_eq;

    let record = make_record(RecordOverrides::default());
    assert_eq!(record.task_id, "st_0000000000000000");
    assert_eq!(record.parent_session_id, "session-parent");
    assert_eq!(record.root_session_id, "session-root");
    assert_eq!(record.depth, 0);
    assert_eq!(record.status, TaskStatus::Running);
    assert_eq!(record.residency_state, ResidencyState::Resident);
    assert_eq!(record.execution_mode, "in-process");
    assert_eq!(record.model, "claude-sonnet-4-5");
    assert_eq!(record.created_at, "2024-12-03T14:00:00.000Z");
    assert_eq!(record.updated_at, "2024-12-03T14:00:00.000Z");
    assert_eq!(record.notification.run_epoch, 0);
    assert_eq!(record.notification.notified_epoch, -1);
    assert_eq!(record.final_response, None);
    assert_eq!(record.description, None);
}

#[test]
fn make_record_applies_overrides() {
    use pretty_assertions::assert_eq;

    let record = make_record(RecordOverrides {
        task_id: Some("st_1".to_string()),
        updated_at: Some("2025-01-01T00:00:00.000Z".to_string()),
        status: Some(TaskStatus::Completed),
        final_response: Some("ok".to_string()),
        ..RecordOverrides::default()
    });
    assert_eq!(record.task_id, "st_1");
    assert_eq!(record.created_at, "2025-01-01T00:00:00.000Z");
    assert_eq!(record.status, TaskStatus::Completed);
    assert_eq!(record.final_response.as_deref(), Some("ok"));
}
