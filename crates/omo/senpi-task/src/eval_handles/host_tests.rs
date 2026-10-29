use super::control::result_of;
use super::errors::EvalHandleErrorCode;
use super::host::{HandleKind, HandleOutcome, HandleRef, TaskStatus};
use super::pool_refs::{self, PoolRecord, PoolStatus};
use super::run_after_engine::{RunAfterEngine, Verdict};
use super::task_refs::{task_snapshot, TaskRecord};
use super::update_queue::UpdateQueue;
use super::watch::HandleWatch;
use std::collections::HashMap;

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

#[test]
fn completed_watch_has_one_terminal_update() {
    let reference = HandleRef { kind: HandleKind::Agent, id: "task-1".into(), run_epoch: 0 };
    let mut watch = HandleWatch { initial: vec![task_snapshot(&record(TaskStatus::Running, 0), reference.clone(), "running")], updates: UpdateQueue::default() };
    let snapshot = task_snapshot(&record(TaskStatus::Completed, 0), reference.clone(), "completed");
    watch.offer(reference, snapshot, &mut HashMap::new());
    assert!(watch.updates.take_next().is_some());
    assert!(watch.updates.take_next().is_none());
}

#[test]
fn watch_updates_preserve_completion_arrival_order() {
    let a = HandleRef { kind: HandleKind::Agent, id: "a".into(), run_epoch: 0 };
    let b = HandleRef { kind: HandleKind::Agent, id: "b".into(), run_epoch: 0 };
    let mut queue = UpdateQueue::default();
    queue.push(task_snapshot(&record(TaskStatus::Completed, 0), b, "b"));
    queue.push(task_snapshot(&record(TaskStatus::Completed, 0), a, "a"));
    assert_eq!(queue.take_next().map(|s| s.host_status), Some("b".into()));
    assert_eq!(queue.take_next().map(|s| s.host_status), Some("a".into()));
}

#[test]
fn completed_record_is_terminal_without_pending_phase() {
    let reference = HandleRef { kind: HandleKind::Agent, id: "task-1".into(), run_epoch: 0 };
    let snapshot = task_snapshot(&record(TaskStatus::Completed, 0), reference, "completed");
    assert_eq!(snapshot.phase, super::host::HandlePhase::Succeeded);
}

#[test]
fn runtime_fallback_moves_to_a_new_epoch() {
    let run = RunAfterEngine::new(1, 0);
    assert_eq!(run.verdict, Verdict::Moved);
    assert_eq!(run.ref_for(&HandleRef { kind: HandleKind::Agent, id: "task-1".into(), run_epoch: 0 }).run_epoch, 1);
}

#[test]
fn stale_reference_cannot_read_successor_run() {
    let reference = HandleRef { kind: HandleKind::Agent, id: "task-1".into(), run_epoch: 0 };
    let err = result_of(&[record(TaskStatus::Running, 1)], reference, "parent-1").unwrap_err();
    assert_eq!(err.code, EvalHandleErrorCode::Stale);
}

#[test]
fn live_cancelled_task_reports_cancelled_outcome() {
    let reference = HandleRef { kind: HandleKind::Agent, id: "task-1".into(), run_epoch: 0 };
    let outcome = result_of(&[record(TaskStatus::Cancelled, 0)], reference, "parent-1").unwrap();
    assert!(matches!(outcome, HandleOutcome::Rejected { code: EvalHandleErrorCode::Cancelled, .. }));
}

#[test]
fn pre_upgrade_epoch_is_rejected_as_stale() {
    let reference = HandleRef { kind: HandleKind::Agent, id: "task-1".into(), run_epoch: 0 };
    let err = result_of(&[record(TaskStatus::Completed, 1)], reference, "parent-1").unwrap_err();
    assert_eq!(err.code, EvalHandleErrorCode::Stale);
}

#[test]
fn unknown_task_is_not_found() {
    let reference = HandleRef { kind: HandleKind::Agent, id: "missing".into(), run_epoch: 0 };
    let err = result_of(&[], reference, "parent-1").unwrap_err();
    assert_eq!(err.code, EvalHandleErrorCode::NotFound);
}

#[test]
fn other_session_is_forbidden_for_result_reads() {
    let reference = HandleRef { kind: HandleKind::Agent, id: "task-1".into(), run_epoch: 0 };
    let err = result_of(&[record(TaskStatus::Completed, 0)], reference, "someone-else").unwrap_err();
    assert_eq!(err.code, EvalHandleErrorCode::Forbidden);
}

#[test]
fn watch_drops_duplicate_revision_updates() {
    let reference = HandleRef { kind: HandleKind::Agent, id: "task-1".into(), run_epoch: 0 };
    let snapshot = task_snapshot(&record(TaskStatus::Completed, 0), reference.clone(), "completed");
    let mut watch = HandleWatch { initial: Vec::new(), updates: UpdateQueue::default() };
    let mut latest = HashMap::new();
    watch.offer(reference.clone(), snapshot.clone(), &mut latest);
    watch.offer(reference, snapshot, &mut latest);
    assert!(watch.updates.take_next().is_some());
    assert!(watch.updates.take_next().is_none());
}

#[test]
fn rollback_is_distinguished_from_a_newer_run() {
    assert_eq!(RunAfterEngine::new(0, 1).verdict, Verdict::RolledBack);
}

#[test]
fn unchanged_run_is_live_after_engine_operation() {
    assert_eq!(RunAfterEngine::new(0, 0).verdict, Verdict::Live);
}

#[test]
fn closing_empty_pool_is_succeeded() {
    let pool = PoolRecord { id: "pool-1".into(), owner_session_id: "parent-1".into(), status: PoolStatus::Closing, item_count: 0 };
    let snapshot = pool_refs::snapshot(&pool, HandleRef { kind: HandleKind::Workpool, id: pool.id.clone(), run_epoch: 0 });
    assert_eq!(snapshot.phase, super::host::HandlePhase::Succeeded);
}

#[test]
fn open_pool_is_pending() {
    let pool = PoolRecord { id: "pool-1".into(), owner_session_id: "parent-1".into(), status: PoolStatus::Open, item_count: 0 };
    assert!(!pool_refs::settled(&pool));
}

#[test]
fn cancelled_pool_reports_cancelled_phase() {
    let pool = PoolRecord { id: "pool-1".into(), owner_session_id: "parent-1".into(), status: PoolStatus::Cancelled, item_count: 0 };
    let snapshot = pool_refs::snapshot(&pool, HandleRef { kind: HandleKind::Workpool, id: pool.id.clone(), run_epoch: 0 });
    assert_eq!(snapshot.phase, super::host::HandlePhase::Cancelled);
}

#[test]
fn send_revived_branch_reports_follow_up_epoch() {
    let run = RunAfterEngine::new(2, 0);
    assert_eq!(run.ref_for(&HandleRef { kind: HandleKind::Agent, id: "task-1".into(), run_epoch: 0 }).run_epoch, 2);
}

#[test]
fn send_steered_branch_reports_follow_up_epoch() {
    assert_eq!(RunAfterEngine::new(1, 0).phase(), super::host::HandlePhase::Pending);
}

#[test]
fn send_queued_branch_reports_follow_up_epoch() {
    let run = RunAfterEngine::new(1, 0);
    assert_eq!(run.verdict, Verdict::Moved);
}

#[test]
fn update_queue_ends_only_after_buffer_drains() {
    let mut queue = UpdateQueue::default();
    queue.push(task_snapshot(&record(TaskStatus::Completed, 0), HandleRef { kind: HandleKind::Agent, id: "task-1".into(), run_epoch: 0 }, "done"));
    queue.end();
    assert!(!queue.is_ended());
    assert!(queue.take_next().is_some());
    assert!(queue.is_ended());
}

// ---- eval-handles/run-after-engine.ts `delivered` / `queued` ----

fn send_ref() -> HandleRef {
    HandleRef { kind: HandleKind::Agent, id: "task-1".into(), run_epoch: 0 }
}

#[test]
fn send_delivered_reports_the_delivery_status_verbatim() {
    let snapshot = RunAfterEngine::new(1, 1)
        .delivered(&send_ref(), &record(TaskStatus::Running, 1), Some("held until the run resumes"))
        .expect("delivered");
    assert_eq!(snapshot.host_status, "held until the run resumes");
    assert_eq!(snapshot.reference.run_epoch, 1);
    assert_eq!(snapshot.phase, super::host::HandlePhase::Pending);
}

#[test]
fn send_delivered_after_the_run_ended_reports_delivered_after_run_ended() {
    let snapshot = RunAfterEngine::new(0, 0)
        .delivered(&send_ref(), &record(TaskStatus::Completed, 0), None)
        .expect("delivered");
    assert_eq!(snapshot.host_status, "delivered_after_run_ended");
    assert_eq!(snapshot.phase, super::host::HandlePhase::Succeeded);
}

#[test]
fn send_queued_stays_queued_even_when_the_run_ended() {
    let snapshot = RunAfterEngine::new(0, 0)
        .queued(&send_ref(), &record(TaskStatus::Completed, 0))
        .expect("queued");
    assert_eq!(snapshot.host_status, "queued");
    assert_eq!(snapshot.phase, super::host::HandlePhase::Succeeded);
}

#[test]
fn send_refuses_a_run_that_was_rolled_back() {
    let error = RunAfterEngine::new(0, 1)
        .delivered(&send_ref(), &record(TaskStatus::Running, 1), None)
        .expect_err("rolled back");
    assert_eq!(error.code, EvalHandleErrorCode::SendRefused);
}

#[test]
fn send_moved_run_answers_with_the_newer_epoch() {
    let snapshot = RunAfterEngine::new(2, 0)
        .queued(&send_ref(), &record(TaskStatus::Running, 2))
        .expect("queued");
    assert_eq!(snapshot.reference.run_epoch, 2);
    assert_eq!(snapshot.host_status, "continued as epoch 2");
}

// ---- eval-handles/task-refs.ts `PHASES` ----

#[test]
fn a_paused_task_projects_to_the_pending_phase() {
    let snapshot = task_snapshot(&record(TaskStatus::Paused, 3), send_ref(), "paused");
    assert_eq!(snapshot.phase, super::host::HandlePhase::Pending);
    assert_eq!(snapshot.revision, 6);
}
