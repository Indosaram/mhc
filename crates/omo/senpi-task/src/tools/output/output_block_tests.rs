//! `tools/output/output-block.test.ts`

use std::sync::{Arc, Mutex};

use pretty_assertions::assert_eq;
use serde_json::{Value, json};

use crate::manager::types::{ListScope, ListedTask};
use crate::state::{TaskRecord, TaskStatus};
use crate::tools::output::output::{TaskOutputInput, TaskOutputMode, run_task_output, task_output_params_schema};
use crate::tools::output::records_fakes::{RecordOverrides, make_record};
use crate::tools::output::types::{
    OutputManager, TaskOutputDeps, TaskOutputDetails, TaskOutputToolResult, TranscriptReadResult, TranscriptSource,
};

const BLOCKING_REMOVED_GUIDANCE: &str =
    "blocking removed - completion arrives as a notification; use mode:\"tail\" to peek.";

struct ObservableOutputManager {
    records: Vec<TaskRecord>,
    wait_for_calls: Mutex<Vec<String>>,
}

impl ObservableOutputManager {
    fn wait_for(&self, task_id: &str) -> TaskRecord {
        self.wait_for_calls
            .lock()
            .expect("wait_for_calls lock")
            .push(task_id.to_string());
        self.records
            .iter()
            .find(|record| record.task_id == task_id)
            .cloned()
            .unwrap_or_else(|| {
                make_record(RecordOverrides {
                    task_id: Some(task_id.to_string()),
                    ..RecordOverrides::default()
                })
            })
    }

    fn wait_for_calls(&self) -> Vec<String> {
        self.wait_for_calls.lock().expect("wait_for_calls lock").clone()
    }
}

impl OutputManager for ObservableOutputManager {
    fn get(&self, task_id: &str) -> Option<TaskRecord> {
        self.records.iter().find(|record| record.task_id == task_id).cloned()
    }

    fn list(&self, scope: &ListScope) -> Vec<ListedTask> {
        let filtered: Vec<TaskRecord> = if let ListScope::ParentSession(session_id) = scope {
            self.records
                .iter()
                .filter(|record| &record.parent_session_id == session_id)
                .cloned()
                .collect()
        } else {
            self.records.clone()
        };
        filtered
            .into_iter()
            .map(|record| ListedTask {
                record,
                queue_position: None,
            })
            .collect()
    }
}

fn manager_from(records: Vec<TaskRecord>) -> Arc<ObservableOutputManager> {
    let manager = Arc::new(ObservableOutputManager {
        records,
        wait_for_calls: Mutex::new(Vec::new()),
    });
    // Keep `wait_for` observable (mirrors the TS fixture); never invoked by task_output.
    let _ = ObservableOutputManager::wait_for;
    manager
}

fn deps_from(manager: &Arc<ObservableOutputManager>) -> TaskOutputDeps {
    let now = chrono::DateTime::parse_from_rfc3339("2024-12-03T15:00:00.000Z")
        .expect("valid timestamp")
        .timestamp_millis();
    TaskOutputDeps {
        manager: manager.clone(),
        state_dir: "/tmp/state".to_string(),
        transcript_reader: Some(Arc::new(|_input| {
            Ok(TranscriptReadResult {
                entries: Vec::new(),
                source: TranscriptSource::None,
                truncated: None,
            })
        })),
        resolve_caller_session_id: None,
        now: Some(Arc::new(move || now)),
    }
}

fn first_text(result: &TaskOutputToolResult) -> String {
    match &result.details {
        TaskOutputDetails::InvalidArguments { reason } => reason.clone(),
        _ => String::new(),
    }
}

fn running_record() -> TaskRecord {
    make_record(RecordOverrides {
        task_id: Some("st_running".to_string()),
        status: Some(TaskStatus::Running),
        ..RecordOverrides::default()
    })
}

fn assert_legacy_param_redirects(params_patch: impl FnOnce(&mut TaskOutputInput)) {
    // given
    let running = running_record();
    let manager = manager_from(vec![running.clone()]);
    let mut params = TaskOutputInput {
        task_id: Some(running.task_id.clone()),
        ..TaskOutputInput::default()
    };
    params_patch(&mut params);

    // when
    let result = run_task_output(&deps_from(&manager), &params, Some("session-parent")).expect("task_output runs");

    // then
    assert_eq!(manager.wait_for_calls(), Vec::<String>::new());
    assert_eq!(result.details.kind(), "invalid_arguments");
    assert_eq!(first_text(&result), BLOCKING_REMOVED_GUIDANCE);
}

#[test]
fn given_the_task_output_schema_when_exposed_to_a_model_then_blocking_controls_are_absent() {
    // when
    let schema = task_output_params_schema();
    let properties = schema.get("properties").and_then(Value::as_object).expect("properties object");

    // then
    assert!(!properties.contains_key("block"));
    assert!(!properties.contains_key("timeout_ms"));
    assert!(!properties.contains_key("wait_for"));
}

#[test]
fn given_a_running_child_when_task_output_reads_its_status_then_it_returns_its_running_snapshot_without_waiting() {
    // given
    let running = running_record();
    let manager = manager_from(vec![running.clone()]);

    // when
    let params = TaskOutputInput {
        task_id: Some(running.task_id.clone()),
        mode: Some(TaskOutputMode::Status),
        ..TaskOutputInput::default()
    };
    let result = run_task_output(&deps_from(&manager), &params, Some("session-parent")).expect("task_output runs");

    // then
    assert_eq!(manager.wait_for_calls(), Vec::<String>::new());
    assert_eq!(result.details.kind(), "status");
    if let TaskOutputDetails::Status { snapshot } = &result.details {
        assert_eq!(snapshot.status, TaskStatus::Running);
    }
}

#[test]
fn given_a_legacy_block_true_param_when_task_output_runs_then_it_redirects_to_notification_driven_peeks() {
    assert_legacy_param_redirects(|params| params.block = Some(json!(true)));
}

#[test]
fn given_a_legacy_block_false_param_when_task_output_runs_then_it_redirects_to_notification_driven_peeks() {
    assert_legacy_param_redirects(|params| params.block = Some(json!(false)));
}

#[test]
fn given_a_legacy_blocking_timeout_param_when_task_output_runs_then_it_redirects_to_notification_driven_peeks() {
    assert_legacy_param_redirects(|params| params.timeout_ms = Some(json!(1)));
}
