use super::control::result_of;
use super::errors::EvalHandleErrorCode;
use super::host::{HandleKind, HandleOutcome, HandleRef, TaskStatus};
use super::task_refs::{task_snapshot, TaskRecord};

fn record(status: TaskStatus, epoch: i64) -> TaskRecord {
    TaskRecord { id: "task-1".into(), owner_session_id: "parent-1".into(), status, run_epoch: epoch, final_response: Some("done".into()), error_message: None }
}

#[test]
fn a_pending_task_reports_pending() {
    let err = result_of(&[record(TaskStatus::Running, 0)], HandleRef { kind: HandleKind::Agent, id: "task-1".into(), run_epoch: 0 }, "parent-1").unwrap_err();
    assert_eq!(err.code, EvalHandleErrorCode::Pending);
}

#[test]
fn a_completed_task_returns_final_text() {
    let reference = HandleRef { kind: HandleKind::Agent, id: "task-1".into(), run_epoch: 0 };
    assert_eq!(result_of(&[record(TaskStatus::Completed, 0)], reference, "parent-1"), Ok(HandleOutcome::Fulfilled { reference: HandleRef { kind: HandleKind::Agent, id: "task-1".into(), run_epoch: 0 }, value: "done".into() }));
}

#[test]
fn another_owner_is_forbidden() {
    let err = result_of(&[record(TaskStatus::Completed, 0)], HandleRef { kind: HandleKind::Agent, id: "task-1".into(), run_epoch: 0 }, "someone-else").unwrap_err();
    assert_eq!(err.code, EvalHandleErrorCode::Forbidden);
}

#[test]
fn newer_epoch_is_stale() {
    let reference = HandleRef { kind: HandleKind::Agent, id: "task-1".into(), run_epoch: 0 };
    let err = result_of(&[record(TaskStatus::Running, 1)], reference, "parent-1").unwrap_err();
    assert_eq!(err.code, EvalHandleErrorCode::Stale);
}

#[test]
fn task_revision_changes_once_when_settled() {
    let reference = HandleRef { kind: HandleKind::Agent, id: "task-1".into(), run_epoch: 2 };
    assert_eq!(task_snapshot(&record(TaskStatus::Running, 2), reference.clone(), "running").revision, 4);
    assert_eq!(task_snapshot(&record(TaskStatus::Completed, 2), reference, "completed").revision, 5);
}
