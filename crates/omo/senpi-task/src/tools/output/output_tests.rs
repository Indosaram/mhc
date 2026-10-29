//! `tools/output/output.test.ts`

use std::io;
use std::sync::Arc;

use pretty_assertions::assert_eq;

use crate::manager::types::{ListScope, ListedTask};
use crate::state::{ResidencyState, ResolvedModelRecord, ResolvedModelSource, TaskRecord, TaskStatus};
use crate::tools::output::output::{TaskOutputInput, TaskOutputMode, run_task_output};
use crate::tools::output::records_fakes::{RecordOverrides, make_record};
use crate::tools::output::types::{
    OutputManager, TaskOutputDeps, TaskOutputDetails, TaskOutputToolResult, TranscriptEntry,
    TranscriptReadResult, TranscriptReader, TranscriptReaderInput, TranscriptSource,
};

struct FakeManager {
    records: Vec<TaskRecord>,
}

impl OutputManager for FakeManager {
    fn get(&self, task_id: &str) -> Option<TaskRecord> {
        self.records.iter().find(|record| record.task_id == task_id).cloned()
    }

    fn list(&self, scope: &ListScope) -> Vec<ListedTask> {
        self.records
            .iter()
            .filter(|record| match scope {
                ListScope::ParentSession(session_id) => &record.parent_session_id == session_id,
                #[allow(unreachable_patterns)]
                _ => true,
            })
            .map(|record| ListedTask {
                record: record.clone(),
                queue_position: None,
            })
            .collect()
    }
}

fn empty_reader() -> TranscriptReader {
    Arc::new(|_input: &TranscriptReaderInput<'_>| -> io::Result<TranscriptReadResult> {
        Ok(TranscriptReadResult {
            entries: Vec::new(),
            source: TranscriptSource::None,
            truncated: None,
        })
    })
}

fn deps_from(records: Vec<TaskRecord>, reader: Option<TranscriptReader>) -> TaskOutputDeps {
    let now = chrono::DateTime::parse_from_rfc3339("2024-12-03T15:00:00.000Z")
        .expect("valid timestamp")
        .timestamp_millis();
    TaskOutputDeps {
        manager: Arc::new(FakeManager { records }),
        state_dir: "/tmp/state".to_string(),
        transcript_reader: Some(reader.unwrap_or_else(empty_reader)),
        resolve_caller_session_id: None,
        now: Some(Arc::new(move || now)),
    }
}

fn first_text(result: &TaskOutputToolResult) -> String {
    let value = serde_json::to_value(result).expect("serializable result");
    let first = &value["content"][0];
    if first["type"].as_str() == Some("text") {
        first["text"].as_str().unwrap_or_default().to_string()
    } else {
        String::new()
    }
}

fn by_id(task_id: &str, mode: Option<TaskOutputMode>) -> TaskOutputInput {
    TaskOutputInput {
        task_id: Some(task_id.to_string()),
        mode,
        ..TaskOutputInput::default()
    }
}

fn status(value: &str) -> TaskStatus {
    TaskStatus::parse(value).expect("known status")
}

fn residency(value: &str) -> ResidencyState {
    ResidencyState::parse(value).expect("known residency state")
}

#[test]
fn given_a_completed_task_in_tail_mode_when_read_then_the_last_assistant_text_is_present() {
    // given
    let record = make_record(RecordOverrides {
        task_id: Some("st_done".to_string()),
        status: Some(status("completed")),
        final_response: Some("all done".to_string()),
        ..RecordOverrides::default()
    });
    let reader: TranscriptReader = Arc::new(|_input: &TranscriptReaderInput<'_>| {
        Ok(TranscriptReadResult {
            entries: vec![
                TranscriptEntry::Assistant {
                    text: "starting the work".to_string(),
                },
                TranscriptEntry::Tool {
                    tool: "bash".to_string(),
                    is_error: false,
                },
                TranscriptEntry::Assistant {
                    text: "finished the work".to_string(),
                },
            ],
            source: TranscriptSource::EventLog,
            truncated: None,
        })
    });
    let deps = deps_from(vec![record], Some(reader));

    // when
    let result = run_task_output(&deps, &by_id("st_done", Some(TaskOutputMode::Tail)), Some("session-parent"))
        .expect("read succeeds");

    // then
    assert_eq!(result.details.kind(), "transcript");
    let TaskOutputDetails::Transcript { transcript, source, .. } = &result.details else {
        panic!("expected transcript details");
    };
    assert!(transcript.contains("finished the work"));
    assert_eq!(source.as_str(), "event-log");
}

#[test]
fn given_a_source_truncated_transcript_when_read_then_the_rpc_visible_result_reports_truncation() {
    let record = make_record(RecordOverrides {
        task_id: Some("st_source_truncated".to_string()),
        status: Some(status("completed")),
        ..RecordOverrides::default()
    });
    let reader: TranscriptReader = Arc::new(|_input: &TranscriptReaderInput<'_>| {
        Ok(TranscriptReadResult {
            entries: vec![TranscriptEntry::Assistant {
                text: "bounded transcript".to_string(),
            }],
            source: TranscriptSource::EventLog,
            truncated: Some(true),
        })
    });
    let task_id = record.task_id.clone();
    let deps = deps_from(vec![record], Some(reader));

    let result = run_task_output(&deps, &by_id(&task_id, Some(TaskOutputMode::Full)), Some("session-parent"))
        .expect("read succeeds");

    assert_eq!(result.details.kind(), "transcript");
    let TaskOutputDetails::Transcript {
        transcript, truncated, ..
    } = &result.details
    else {
        panic!("expected transcript details");
    };
    assert_eq!(transcript, "assistant: bounded transcript");
    assert!(*truncated);
}

#[test]
fn given_default_mode_when_read_then_a_status_snapshot_with_final_response_is_returned() {
    // given
    let record = make_record(RecordOverrides {
        task_id: Some("st_done".to_string()),
        status: Some(status("completed")),
        final_response: Some("the answer".to_string()),
        ..RecordOverrides::default()
    });
    let deps = deps_from(vec![record], None);

    // when
    let result = run_task_output(&deps, &by_id("st_done", None), Some("session-parent")).expect("read succeeds");

    // then
    assert_eq!(result.details.kind(), "status");
    let TaskOutputDetails::Status { snapshot } = &result.details else {
        panic!("expected status details");
    };
    assert_eq!(snapshot.final_response.as_deref(), Some("the answer"));
    assert_eq!(snapshot.status.as_str(), "completed");
}

#[test]
fn given_a_task_with_a_resolved_model_when_read_then_status_uses_display_plus_reasoning_details() {
    // given
    let mut resolved_model = ResolvedModelRecord::new(ResolvedModelSource::Category, "openai", "gpt-5.6-sol");
    resolved_model.display = "GPT-5.6 Sol".to_string();
    resolved_model.reasoning_effort = Some("high".to_string());
    resolved_model.variant = Some("xhigh".to_string());
    let record = make_record(RecordOverrides {
        task_id: Some("st_resolved".to_string()),
        model: Some("openai/gpt-5.6-sol".to_string()),
        status: Some(status("completed")),
        resolved_model: Some(resolved_model.clone()),
        ..RecordOverrides::default()
    });
    let deps = deps_from(vec![record], None);

    // when
    let result = run_task_output(&deps, &by_id("st_resolved", None), Some("session-parent")).expect("read succeeds");

    // then
    let text = first_text(&result);
    assert!(text.contains("model GPT-5.6 Sol (reasoning high, variant xhigh)"));
    assert!(!text.contains("model openai/gpt-5.6-sol"));
    assert_eq!(result.details.kind(), "status");
    let TaskOutputDetails::Status { snapshot } = &result.details else {
        panic!("expected status details");
    };
    assert_eq!(snapshot.resolved_model, Some(resolved_model));
}

#[test]
fn given_a_task_without_a_resolved_model_when_read_then_status_keeps_raw_model_fallback() {
    // given
    let record = make_record(RecordOverrides {
        task_id: Some("st_raw".to_string()),
        model: Some("anthropic/claude-sonnet-4-5".to_string()),
        status: Some(status("completed")),
        ..RecordOverrides::default()
    });
    let deps = deps_from(vec![record], None);

    // when
    let result = run_task_output(&deps, &by_id("st_raw", None), Some("session-parent")).expect("read succeeds");

    // then
    let text = first_text(&result);
    assert!(text.contains("model anthropic/claude-sonnet-4-5"));
    assert!(!text.contains("reasoning"));
    assert!(!text.contains("variant"));
}

#[test]
fn given_a_lost_task_when_read_then_a_status_view_with_lost_breadcrumbs_is_returned_without_throwing() {
    // given
    let record = make_record(RecordOverrides {
        task_id: Some("st_lost".to_string()),
        status: Some(status("lost")),
        pid: Some(4242),
        ..RecordOverrides::default()
    });
    let deps = deps_from(vec![record], None);

    // when
    let result = run_task_output(&deps, &by_id("st_lost", Some(TaskOutputMode::Tail)), Some("session-parent"))
        .expect("read succeeds");

    // then
    assert_eq!(result.details.kind(), "status");
    let TaskOutputDetails::Status { snapshot } = &result.details else {
        panic!("expected status details");
    };
    let lost = snapshot.lost.as_ref().expect("lost breadcrumbs");
    assert_eq!(lost.pid, Some(4242));
    assert!(lost.session_dir.contains("st_lost"));
    assert!(!lost.explanation.is_empty());
}

#[test]
fn given_a_task_owned_by_another_session_when_read_then_it_is_not_found() {
    // given
    let record = make_record(RecordOverrides {
        task_id: Some("st_other".to_string()),
        parent_session_id: Some("session-other".to_string()),
        ..RecordOverrides::default()
    });
    let deps = deps_from(vec![record], None);

    // when
    let result = run_task_output(&deps, &by_id("st_other", Some(TaskOutputMode::Status)), Some("session-parent"))
        .expect("read succeeds");

    // then
    assert_eq!(result.details.kind(), "not_found");
}

#[test]
fn given_no_caller_session_when_read_then_it_fails_closed_as_not_found() {
    // given
    let record = make_record(RecordOverrides {
        task_id: Some("st_a".to_string()),
        parent_session_id: Some("session-parent".to_string()),
        ..RecordOverrides::default()
    });
    let deps = deps_from(vec![record], None);

    // when
    let result =
        run_task_output(&deps, &by_id("st_a", Some(TaskOutputMode::Status)), None).expect("read succeeds");

    // then
    assert_eq!(result.details.kind(), "not_found");
}

#[test]
fn given_neither_task_id_nor_name_when_read_then_invalid_arguments_are_reported() {
    // given
    let deps = deps_from(Vec::new(), None);

    // when
    let params = TaskOutputInput {
        mode: Some(TaskOutputMode::Status),
        ..TaskOutputInput::default()
    };
    let result = run_task_output(&deps, &params, Some("session-parent")).expect("read succeeds");

    // then
    assert_eq!(result.details.kind(), "invalid_arguments");
}

#[test]
fn given_a_name_instead_of_an_id_when_read_then_the_task_is_resolved_by_name() {
    // given
    let record = make_record(RecordOverrides {
        task_id: Some("st_named".to_string()),
        name: Some("explorer".to_string()),
        status: Some(status("completed")),
        final_response: Some("found".to_string()),
        ..RecordOverrides::default()
    });
    let deps = deps_from(vec![record], None);

    // when
    let params = TaskOutputInput {
        name: Some("explorer".to_string()),
        mode: Some(TaskOutputMode::Status),
        ..TaskOutputInput::default()
    };
    let result = run_task_output(&deps, &params, Some("session-parent")).expect("read succeeds");

    // then
    assert_eq!(result.details.kind(), "status");
    let TaskOutputDetails::Status { snapshot } = &result.details else {
        panic!("expected status details");
    };
    assert_eq!(snapshot.task_id, "st_named");
}

#[test]
fn given_a_persisted_only_record_when_read_in_status_mode_then_the_status_text_states_it_is_suspended() {
    // given
    let record = make_record(RecordOverrides {
        task_id: Some("st_susp".to_string()),
        status: Some(status("running")),
        residency_state: Some(residency("persisted_only")),
        ..RecordOverrides::default()
    });
    let deps = deps_from(vec![record], None);

    // when
    let result = run_task_output(&deps, &by_id("st_susp", None), Some("session-parent")).expect("read succeeds");

    // then
    assert_eq!(result.details.kind(), "status");
    let TaskOutputDetails::Status { snapshot } = &result.details else {
        panic!("expected status details");
    };
    assert_eq!(snapshot.residency_state.as_str(), "persisted_only");
    assert!(snapshot.suspended.is_some());
    assert!(first_text(&result).contains("suspended"));
}

#[test]
fn given_an_rpc_detached_record_when_read_in_status_mode_then_the_status_text_states_it_is_suspended() {
    // given
    let record = make_record(RecordOverrides {
        task_id: Some("st_susp".to_string()),
        status: Some(status("running")),
        residency_state: Some(residency("rpc_detached")),
        ..RecordOverrides::default()
    });
    let deps = deps_from(vec![record], None);

    // when
    let result = run_task_output(&deps, &by_id("st_susp", None), Some("session-parent")).expect("read succeeds");

    // then
    assert_eq!(result.details.kind(), "status");
    let TaskOutputDetails::Status { snapshot } = &result.details else {
        panic!("expected status details");
    };
    assert_eq!(snapshot.residency_state.as_str(), "rpc_detached");
    assert!(snapshot.suspended.is_some());
    assert!(first_text(&result).contains("suspended"));
}

#[test]
fn given_a_resident_record_when_read_in_status_mode_then_no_suspended_text_appears() {
    // given
    let record = make_record(RecordOverrides {
        task_id: Some("st_live".to_string()),
        status: Some(status("running")),
        residency_state: Some(residency("resident")),
        ..RecordOverrides::default()
    });
    let deps = deps_from(vec![record], None);

    // when
    let result = run_task_output(&deps, &by_id("st_live", None), Some("session-parent")).expect("read succeeds");

    // then
    assert!(!first_text(&result).contains("suspended"));
}
