//! `manager/manager-continue.test.ts` and `manager/manager-outcome.test.ts`.

use std::sync::{Arc, Mutex};

use super::fakes::{
    Harness, HarnessOptions, base_spec, config, default_manager, lock, make_manager, named,
    started, status_of, wait_terminal, wait_until,
};
use crate::agents::interaction_policy_for_agent;
use crate::manager::continue_result::{ContinueDelivery, ContinueResult};
use crate::manager::types::{ListScope, ManagerStartSpec};
use crate::runners::{RunnerFailureKind, RunnerOutcome};
use crate::state::{DeliverAs, ResidencyState, TaskRecord, TaskStatus, TaskTransition};
use crate::steering::{SendInput, SendOutcome};
use crate::store::{StateDirConfig, TaskRecordStore};

fn iso() -> String {
    chrono::Utc::now().to_rfc3339()
}

fn transition(harness: &Harness, task_id: &str, transition: &TaskTransition) {
    harness
        .store
        .transition(task_id, transition)
        .expect("transition");
}

fn record(harness: &Harness, task_id: &str) -> TaskRecord {
    harness.store.load(task_id).expect("load").expect("record")
}

/// Settles the fake and blocks until the manager's tracker has applied or ignored the outcome.
fn settle_and_flush(harness: &Harness, task_id: &str, outcome: RunnerOutcome) {
    let before = harness.manager.processed_outcomes();
    harness.in_process.wait_handle(task_id).settle(outcome);
    wait_until("outcome processed", || {
        harness.manager.processed_outcomes() > before
    });
}

// manager-continue.test.ts

#[test]
fn given_running_resident_child_when_continued_with_steer_then_steer_delivered_via_handle() {
    let harness = default_manager();
    let task = started(harness.manager.start(&base_spec()));
    let fake = harness.in_process.wait_handle(&task.task_id);

    let result = harness
        .manager
        .continue_task(&task.task_id, "keep going", Some(DeliverAs::Steer))
        .expect("continue");

    let ContinueResult::Continued { delivered, .. } = result else {
        panic!("expected continued, got {result:?}");
    };
    assert_eq!(delivered, ContinueDelivery::Steer);
    assert_eq!(fake.steer_calls(), vec!["keep going".to_string()]);
}

#[test]
fn given_running_resident_child_when_continued_without_delivery_then_follow_up_is_default() {
    let harness = default_manager();
    let task = started(harness.manager.start(&base_spec()));
    let fake = harness.in_process.wait_handle(&task.task_id);

    let result = harness
        .manager
        .continue_task(&task.task_id, "more context", None)
        .expect("continue");

    let ContinueResult::Continued { delivered, .. } = result else {
        panic!("expected continued, got {result:?}");
    };
    assert_eq!(delivered, ContinueDelivery::FollowUp);
    assert_eq!(fake.follow_up_calls(), vec!["more context".to_string()]);
}

#[test]
fn given_completed_resident_child_when_continued_then_revived_on_same_handle_with_incremented_epoch()
 {
    let harness = default_manager();
    let task = started(harness.manager.start(&base_spec()));
    let fake = harness.in_process.wait_handle(&task.task_id);
    fake.complete("first pass");
    wait_terminal(&harness.manager, &task.task_id);
    assert_eq!(
        status_of(&harness.store, &task.task_id),
        Some(TaskStatus::Completed)
    );

    let result = harness
        .manager
        .continue_task(&task.task_id, "second pass", None)
        .expect("continue");

    let ContinueResult::Continued { delivered, .. } = result else {
        panic!("expected continued, got {result:?}");
    };
    assert_eq!(delivered, ContinueDelivery::Revive);
    assert_eq!(fake.follow_up_calls(), vec!["second pass".to_string()]);
    let reloaded = record(&harness, &task.task_id);
    assert_eq!(reloaded.status, TaskStatus::Running);
    assert_eq!(reloaded.notification.run_epoch, 1);
}

#[test]
fn given_cancelled_child_when_continued_then_not_continuable_and_suggests_task_output() {
    let harness = default_manager();
    let task = started(harness.manager.start(&base_spec()));
    transition(
        &harness,
        &task.task_id,
        &TaskTransition::Cancel {
            timestamp: iso(),
            error_message: None,
            run_stats: None,
        },
    );

    let result = harness
        .manager
        .continue_task(&task.task_id, "hello", None)
        .expect("continue");

    let ContinueResult::NotContinuable { suggestion, .. } = result else {
        panic!("expected not continuable, got {result:?}");
    };
    // `task_output` is the tool name the suggestion routes the caller to.
    assert!(suggestion.contains("task_output"), "{suggestion}");
}

#[test]
fn given_unknown_task_id_when_continued_then_not_continuable() {
    let harness = default_manager();

    let result = harness
        .manager
        .continue_task("st_0000dead", "hello", None)
        .expect("continue");

    assert!(matches!(result, ContinueResult::NotContinuable { .. }));
}

#[test]
fn given_momus_child_started_through_manager_when_sent_then_outcome_is_one_shot_agent() {
    let harness = default_manager();
    let task = started(harness.manager.start(&ManagerStartSpec {
        subagent_type: Some("momus".to_string()),
        ..base_spec()
    }));
    let fake = harness.in_process.wait_handle(&task.task_id);

    let outcome = harness
        .manager
        .send_to_task(&SendInput::new(task.task_id.clone(), "mid-run note"))
        .expect("send");

    let SendOutcome::OneShotAgent {
        task_id,
        agent,
        message,
    } = outcome
    else {
        panic!("expected one-shot agent, got {outcome:?}");
    };
    assert_eq!(task_id, task.task_id);
    assert_eq!(agent, "momus");
    let policy = interaction_policy_for_agent("momus").expect("momus policy");
    assert_eq!(message, policy.send_denial_reminder);
    assert!(fake.follow_up_calls().is_empty());
    assert!(fake.steer_calls().is_empty());
}

#[test]
fn given_tasks_across_two_parents_when_listing_parent_scope_then_only_that_parents_tasks_appear() {
    let harness = default_manager();
    let mine = started(harness.manager.start(&ManagerStartSpec {
        parent_session_id: "parent-1".to_string(),
        ..named("mine")
    }));
    started(harness.manager.start(&ManagerStartSpec {
        parent_session_id: "parent-2".to_string(),
        ..named("theirs")
    }));

    let listed = harness
        .manager
        .list(&ListScope::ParentSession("parent-1".to_string()));

    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].record.task_id, mine.task_id);
}

#[test]
fn given_queued_task_when_listing_then_queue_position_exposed() {
    let harness = make_manager(HarnessOptions {
        config: Some(config(1, 1)),
        ..HarnessOptions::default()
    });
    started(harness.manager.start(&named("a")));
    let queued = started(harness.manager.start(&named("b")));

    let listed = harness.manager.list(&ListScope::All);

    let entry = listed
        .iter()
        .find(|entry| entry.record.task_id == queued.task_id)
        .expect("queued entry");
    assert_eq!(entry.queue_position, Some(1));
}

#[test]
fn given_foreground_task_when_waiting_then_wait_for_resolves_with_terminal_record() {
    let harness = default_manager();
    let task = started(harness.manager.start(&base_spec()));

    harness
        .in_process
        .wait_handle(&task.task_id)
        .complete("the answer");
    let settled = wait_terminal(&harness.manager, &task.task_id);

    assert_eq!(settled.status, TaskStatus::Completed);
    assert_eq!(settled.final_response.as_deref(), Some("the answer"));
}

// manager-outcome.test.ts

#[test]
fn given_suspended_child_when_abort_settles_cancelled_then_record_stays_running_and_no_waiter_settles()
 {
    let harness = default_manager();
    let task = started(harness.manager.start(&base_spec()));
    harness.in_process.wait_handle(&task.task_id);
    let settled: Arc<Mutex<Option<TaskRecord>>> = Arc::default();
    // Detached: the waiter must stay blocked; the test process ends it.
    let _waiter = {
        let manager = harness.manager.clone();
        let task_id = task.task_id.clone();
        let settled = Arc::clone(&settled);
        std::thread::spawn(move || {
            if let Ok(record) = manager.wait_for(&task_id, None, None) {
                *lock(&settled) = Some(record);
            }
        })
    };
    wait_until("waiter registered", || {
        harness.manager.waiter_count(&task.task_id) == 1
    });

    harness.manager.forget(&task.task_id);
    transition(
        &harness,
        &task.task_id,
        &TaskTransition::PersistOnly { timestamp: iso() },
    );
    settle_and_flush(&harness, &task.task_id, RunnerOutcome::Cancelled);

    let reloaded = record(&harness, &task.task_id);
    assert_eq!(reloaded.status, TaskStatus::Running);
    assert_eq!(reloaded.residency_state, ResidencyState::PersistedOnly);
    assert!(lock(&settled).is_none());
    assert_eq!(harness.manager.waiter_key_count(), 1);
}

#[test]
fn given_suspended_child_when_abort_settles_as_error_then_no_error_terminal_lands() {
    let harness = default_manager();
    let task = started(harness.manager.start(&base_spec()));
    harness.in_process.wait_handle(&task.task_id);
    harness.manager.forget(&task.task_id);
    transition(
        &harness,
        &task.task_id,
        &TaskTransition::PersistOnly { timestamp: iso() },
    );

    let before = harness.manager.processed_outcomes();
    harness
        .in_process
        .wait_handle(&task.task_id)
        .fail(RunnerFailureKind::ChildTurnFailed, "aborted");
    wait_until("outcome processed", || {
        harness.manager.processed_outcomes() > before
    });

    let reloaded = record(&harness, &task.task_id);
    assert_eq!(reloaded.status, TaskStatus::Running);
    assert_eq!(reloaded.residency_state, ResidencyState::PersistedOnly);
    assert_eq!(reloaded.error_message, None);
}

#[test]
fn given_suspended_child_when_misleading_success_settles_late_then_record_keeps_running() {
    let harness = default_manager();
    let task = started(harness.manager.start(&base_spec()));
    harness.in_process.wait_handle(&task.task_id);
    harness.manager.forget(&task.task_id);
    transition(
        &harness,
        &task.task_id,
        &TaskTransition::PersistOnly { timestamp: iso() },
    );

    settle_and_flush(
        &harness,
        &task.task_id,
        RunnerOutcome::Completed {
            final_response: "done".to_string(),
        },
    );

    let reloaded = record(&harness, &task.task_id);
    assert_eq!(reloaded.status, TaskStatus::Running);
    assert_eq!(reloaded.residency_state, ResidencyState::PersistedOnly);
    assert_eq!(reloaded.final_response, None);
}

#[test]
fn given_running_child_when_outcome_settles_after_forget_before_persist_only_then_record_untouched()
{
    let harness = default_manager();
    let task = started(harness.manager.start(&base_spec()));
    harness.in_process.wait_handle(&task.task_id);

    harness.manager.forget(&task.task_id);
    settle_and_flush(&harness, &task.task_id, RunnerOutcome::Cancelled);

    let reloaded = record(&harness, &task.task_id);
    assert_eq!(reloaded.status, TaskStatus::Running);
    assert_eq!(reloaded.residency_state, ResidencyState::Resident);
}

#[test]
fn given_record_disposed_while_running_with_handle_mapped_when_abort_settles_then_late_outcome_terminalizes()
 {
    let harness = default_manager();
    let task = started(harness.manager.start(&base_spec()));
    harness.in_process.wait_handle(&task.task_id);
    transition(
        &harness,
        &task.task_id,
        &TaskTransition::Dispose { timestamp: iso() },
    );

    settle_and_flush(&harness, &task.task_id, RunnerOutcome::Cancelled);

    let reloaded = record(&harness, &task.task_id);
    assert_eq!(reloaded.status, TaskStatus::Cancelled);
    assert_eq!(reloaded.residency_state, ResidencyState::Disposed);
}

#[test]
fn given_running_resident_child_when_completes_normally_then_record_completes_and_waiter_settles() {
    let harness = default_manager();
    let task = started(harness.manager.start(&base_spec()));
    let waiter = {
        let manager = harness.manager.clone();
        let task_id = task.task_id.clone();
        std::thread::spawn(move || manager.wait_for(&task_id, None, Some(super::fakes::WAIT)))
    };
    wait_until("waiter registered", || {
        harness.manager.waiter_count(&task.task_id) == 1
    });

    harness
        .in_process
        .wait_handle(&task.task_id)
        .complete("done");

    let settled = waiter.join().expect("waiter thread").expect("terminal");
    assert_eq!(settled.status, TaskStatus::Completed);
    assert_eq!(settled.final_response.as_deref(), Some("done"));
    assert_eq!(
        status_of(&harness.store, &task.task_id),
        Some(TaskStatus::Completed)
    );
}

#[test]
fn given_foreground_child_when_promoted_to_background_then_notify_on_terminal_round_trips() {
    let harness = default_manager();
    let task = started(harness.manager.start(&base_spec()));
    assert!(!record(&harness, &task.task_id).notify_on_terminal);

    let promoted = harness.manager.promote_to_background(&task.task_id);

    assert!(promoted);
    let reloaded = TaskRecordStore::new(&StateDirConfig {
        project_dir: harness.project.dir.path().to_path_buf(),
        task_state_dir: None,
    });
    let persisted = reloaded.load(&task.task_id).expect("load").expect("record");
    assert!(persisted.notify_on_terminal);
    assert!(harness.manager.was_background(&task.task_id));
    assert!(!harness.manager.promote_to_background(&task.task_id));
}

#[test]
fn given_persisted_background_record_when_fresh_manager_asked_then_was_background_reads_record() {
    let first = default_manager();
    let task = started(first.manager.start(&ManagerStartSpec {
        run_in_background: true,
        ..base_spec()
    }));

    let second = make_manager(HarnessOptions {
        project: Some(first.project.clone()),
        ..HarnessOptions::default()
    });

    assert!(second.manager.was_background(&task.task_id));
}

#[test]
fn given_unknown_task_id_promoted_in_memory_when_queried_then_was_background_falls_back_to_memory()
{
    let harness = default_manager();

    let promoted = harness.manager.promote_to_background("st_0000ffff");

    assert!(promoted);
    assert!(harness.manager.was_background("st_0000ffff"));
}
