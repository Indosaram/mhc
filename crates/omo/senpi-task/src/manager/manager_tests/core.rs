//! `manager/manager.test.ts`.

use std::sync::{Arc, Mutex};

use serde_json::json;

use super::fakes::{
    FakeHandle, FakeRunner, HarnessOptions, WAIT, base_spec, category_planner, config,
    default_manager, lock, make_lifecycle_manager, make_manager, named, notify, started, status_of,
    wait_terminal, wait_until,
};
use crate::manager::execution_mode::ExecutionMode;
use crate::manager::types::{
    ChildPlanner, ListScope, ManagedRunner, ManagedRunnerResult, ManagedStartSpec,
    ManagerStartSpec, ResolvedChildPlan, StartResult,
};
use crate::manager::{AbortSignal, WaitError};
use crate::state::{ResidencyState, ResolvedModelRecord, ResolvedModelSource, TaskStatus};
use crate::steering::{CancelOptions, CancelOutcome};

fn model_record(
    provider: &str,
    model_id: &str,
    display: &str,
    variant: Option<&str>,
    reasoning_effort: Option<&str>,
) -> ResolvedModelRecord {
    ResolvedModelRecord {
        display: display.to_string(),
        variant: variant.map(str::to_string),
        reasoning_effort: reasoning_effort.map(str::to_string),
        ..ResolvedModelRecord::new(ResolvedModelSource::Category, provider, model_id)
    }
}

#[test]
fn given_valid_spec_when_started_then_returns_st_id_and_running_status_with_persisted_record() {
    let harness = default_manager();

    let task = started(harness.manager.start(&base_spec()));

    assert!(crate::state::parse_task_id(&task.task_id).is_ok());
    assert_eq!(task.status, TaskStatus::Running);
    assert_eq!(
        status_of(&harness.store, &task.task_id),
        Some(TaskStatus::Running)
    );
}

#[test]
fn given_child_beyond_max_depth_when_started_then_depth_denied_and_zero_records() {
    let harness = make_manager(HarnessOptions {
        config: Some(config(5, 1)),
        ..HarnessOptions::default()
    });

    let result = harness.manager.start(&ManagerStartSpec {
        depth: 2,
        ..base_spec()
    });

    assert_eq!(result.kind(), "depth_denied");
    assert!(harness.store.list().expect("list").records.is_empty());
}

#[test]
fn given_full_model_slot_when_further_task_starts_then_queues_fifo_and_starts_when_slot_frees() {
    let harness = make_manager(HarnessOptions {
        config: Some(config(1, 1)),
        ..HarnessOptions::default()
    });
    let first = started(harness.manager.start(&named("a")));

    let second = started(harness.manager.start(&named("b")));

    assert_eq!(second.status, TaskStatus::Pending);
    assert_eq!(second.queue_position, Some(1));
    harness
        .in_process
        .wait_handle(&first.task_id)
        .complete("ok");
    harness.in_process.wait_handle(&second.task_id);
    wait_until("second running", || {
        status_of(&harness.store, &second.task_id) == Some(TaskStatus::Running)
    });
    assert_eq!(
        wait_terminal(&harness.manager, &first.task_id).status,
        TaskStatus::Completed
    );
}

#[test]
fn given_two_categories_with_different_models_when_both_start_under_limit_one_then_both_run() {
    let harness = make_manager(HarnessOptions {
        planner: Some(category_planner(&[
            ("quick", "anthropic/claude"),
            ("deep", "openai/gpt"),
        ])),
        config: Some(config(1, 1)),
        ..HarnessOptions::default()
    });

    let a = started(harness.manager.start(&ManagerStartSpec {
        category: Some("quick".to_string()),
        ..named("a")
    }));
    let b = started(harness.manager.start(&ManagerStartSpec {
        category: Some("deep".to_string()),
        ..named("b")
    }));

    assert_eq!(a.status, TaskStatus::Running);
    assert_eq!(b.status, TaskStatus::Running);
    assert_eq!(
        status_of(&harness.store, &a.task_id),
        Some(TaskStatus::Running)
    );
    assert_eq!(
        status_of(&harness.store, &b.task_id),
        Some(TaskStatus::Running)
    );
}

#[test]
fn given_runner_whose_start_throws_when_task_starts_then_slot_released_record_error_and_event_logged()
 {
    let runner = FakeRunner::new();
    runner.throw_on_start(true);
    let harness = make_manager(HarnessOptions {
        in_process: Some(Arc::clone(&runner)),
        config: Some(config(1, 1)),
        ..HarnessOptions::default()
    });

    let result = harness.manager.start(&base_spec());

    let StartResult::StartFailed(failure) = result else {
        panic!("expected start_failed, got {result:?}");
    };
    assert_eq!(
        status_of(&harness.store, &failure.task_id),
        Some(TaskStatus::Error)
    );
    let jsonl = std::fs::read_to_string(
        harness
            .store
            .state_dir()
            .join("logs")
            .join(format!("{}.jsonl", failure.task_id)),
    )
    .expect("event log");
    assert!(jsonl.contains("error"));
    runner.throw_on_start(false);
    let next = started(harness.manager.start(&base_spec()));
    assert_eq!(next.status, TaskStatus::Running);
}

#[test]
fn given_requested_name_collision_in_same_parent_when_started_then_suffix_and_warning() {
    let harness = default_manager();
    started(harness.manager.start(&named("reviewer")));

    let second = started(harness.manager.start(&named("reviewer")));

    assert_eq!(second.name, "reviewer-2");
    assert!(second.name_warning.is_some());
}

#[test]
fn given_execution_mode_process_on_spec_when_started_then_process_runner_is_used() {
    let harness = default_manager();

    started(harness.manager.start(&ManagerStartSpec {
        execution_mode: Some(ExecutionMode::Process),
        ..base_spec()
    }));

    assert_eq!(harness.process.started_count(), 1);
    assert_eq!(harness.in_process.started_count(), 0);
}

#[test]
fn given_resolved_model_plan_when_started_then_metadata_persists_resolved_model_without_messages() {
    let resolved_model = model_record(
        "anthropic",
        "claude-sonnet-4-20250514",
        "Claude Sonnet 4",
        Some("sonnet"),
        Some("medium"),
    );
    let plan_model = resolved_model.clone();
    let planner: ChildPlanner = Arc::new(move |spec: &ManagerStartSpec| {
        Ok(ResolvedChildPlan {
            model: spec
                .model
                .clone()
                .unwrap_or_else(|| "anthropic/claude".to_string()),
            resolved_model: Some(plan_model.clone()),
            category: spec.category.clone(),
            ..ResolvedChildPlan::default()
        })
    });
    let harness = make_manager(HarnessOptions {
        planner: Some(planner),
        ..HarnessOptions::default()
    });

    let task = started(harness.manager.start(&ManagerStartSpec {
        prompt: "private prompt payload".to_string(),
        ..base_spec()
    }));

    assert_eq!(task.resolved_model.as_ref(), Some(&resolved_model));
    let persisted = harness
        .store
        .load(&task.task_id)
        .expect("load")
        .expect("record");
    assert_eq!(persisted.resolved_model.as_ref(), Some(&resolved_model));
    assert_eq!(
        harness
            .manager
            .get(&task.task_id)
            .and_then(|record| record.resolved_model),
        Some(resolved_model.clone())
    );
    assert_eq!(
        harness.manager.list(&ListScope::All)[0]
            .record
            .resolved_model,
        Some(resolved_model)
    );
    let raw = std::fs::read_to_string(
        harness
            .store
            .state_dir()
            .join("tasks")
            .join(format!("{}.json", task.task_id)),
    )
    .expect("raw record");
    assert!(raw.contains("\"resolved_model\""));
    assert!(raw.contains("private prompt payload"));
    assert!(!raw.contains("\"messages\""));
}

#[test]
fn given_resolved_ultrabrain_plan_whose_runner_throws_when_start_fails_then_resolved_context_without_prompt()
 {
    let resolved_model = model_record("openai", "gpt-5.6-sol", "GPT-5.6 Sol", None, Some("xhigh"));
    let plan_model = resolved_model.clone();
    let planner: ChildPlanner = Arc::new(move |_spec: &ManagerStartSpec| {
        Ok(ResolvedChildPlan {
            model: "openai/gpt-5.6-sol".to_string(),
            resolved_model: Some(plan_model.clone()),
            category: Some("ultrabrain".to_string()),
            ..ResolvedChildPlan::default()
        })
    });
    let runner = FakeRunner::new();
    runner.throw_on_start(true);
    let harness = make_manager(HarnessOptions {
        planner: Some(planner),
        in_process: Some(runner),
        ..HarnessOptions::default()
    });
    let private_prompt = "private prompt payload";

    let result = harness.manager.start(&ManagerStartSpec {
        prompt: private_prompt.to_string(),
        category: Some("ultrabrain".to_string()),
        run_in_background: true,
        ..base_spec()
    });

    let StartResult::StartFailed(failure) = &result else {
        panic!("expected start_failed, got {result:?}");
    };
    assert_eq!(failure.name, failure.task_id);
    assert_eq!(failure.category.as_deref(), Some("ultrabrain"));
    assert_eq!(failure.execution_mode, ExecutionMode::InProcess);
    assert_eq!(failure.model, "openai/gpt-5.6-sol");
    assert_eq!(failure.resolved_model.as_ref(), Some(&resolved_model));
    assert!(failure.run_in_background);
    assert_eq!(failure.error_message, "Task runner failed to start.");
    assert!(!format!("{result:?}").contains(private_prompt));
}

#[test]
fn given_running_task_with_abortable_wait_when_signal_aborts_then_wait_rejects_and_key_removed() {
    let harness = default_manager();
    let task = started(harness.manager.start(&base_spec()));
    let signal = AbortSignal::default();
    let manager = harness.manager.clone();
    let task_id = task.task_id.clone();
    let wait_signal = signal.clone();
    let waiting = std::thread::spawn(move || manager.wait_for(&task_id, Some(&wait_signal), None));
    wait_until("waiter registered", || {
        harness.manager.waiter_key_count() == 1
    });

    signal.abort();

    assert_eq!(waiting.join().expect("join"), Err(WaitError::Aborted));
    assert_eq!(harness.manager.waiter_key_count(), 0);
}

#[test]
fn given_abortable_wait_that_resolves_terminal_when_signal_aborts_afterwards_then_abort_is_noop() {
    let harness = default_manager();
    let task = started(harness.manager.start(&base_spec()));
    let handle = harness.in_process.wait_handle(&task.task_id);
    let signal = AbortSignal::default();
    let manager = harness.manager.clone();
    let task_id = task.task_id.clone();
    let wait_signal = signal.clone();
    let waiting = std::thread::spawn(move || manager.wait_for(&task_id, Some(&wait_signal), None));
    wait_until("waiter registered", || {
        harness.manager.waiter_key_count() == 1
    });

    handle.complete("done");
    let completed = waiting.join().expect("join").expect("terminal record");
    signal.abort();

    assert_eq!(completed.status, TaskStatus::Completed);
    assert_eq!(harness.manager.waiter_key_count(), 0);
}

#[test]
fn given_pre_aborted_signal_when_wait_for_called_then_rejects_without_registering_waiter() {
    let harness = default_manager();
    let task = started(harness.manager.start(&base_spec()));
    let signal = AbortSignal::default();
    signal.abort();

    let waiting = harness.manager.wait_for(&task.task_id, Some(&signal), None);

    assert_eq!(harness.manager.waiter_key_count(), 0);
    assert_eq!(waiting, Err(WaitError::Aborted));
}

#[test]
fn given_two_concurrent_waiters_when_one_aborts_then_surviving_waiter_resolves_on_settle() {
    let harness = default_manager();
    let task = started(harness.manager.start(&base_spec()));
    let handle = harness.in_process.wait_handle(&task.task_id);
    let signal = AbortSignal::default();
    let (manager, task_id, wait_signal) = (
        harness.manager.clone(),
        task.task_id.clone(),
        signal.clone(),
    );
    let abandoned =
        std::thread::spawn(move || manager.wait_for(&task_id, Some(&wait_signal), None));
    let (manager, task_id) = (harness.manager.clone(), task.task_id.clone());
    let surviving = std::thread::spawn(move || manager.wait_for(&task_id, None, None));
    wait_until("both waiters registered", || {
        harness.manager.waiter_count(&task.task_id) == 2
    });
    assert_eq!(harness.manager.waiter_key_count(), 1);

    signal.abort();

    assert_eq!(abandoned.join().expect("join"), Err(WaitError::Aborted));
    handle.complete("survived");
    let record = surviving.join().expect("join").expect("terminal record");
    assert_eq!(record.final_response.as_deref(), Some("survived"));
    assert_eq!(harness.manager.waiter_key_count(), 0);
}

#[test]
fn given_running_task_and_signal_less_wait_when_task_settles_then_wait_for_resolves() {
    let harness = default_manager();
    let task = started(harness.manager.start(&base_spec()));
    let handle = harness.in_process.wait_handle(&task.task_id);
    let (manager, task_id) = (harness.manager.clone(), task.task_id.clone());
    let waiting = std::thread::spawn(move || manager.wait_for(&task_id, None, None));
    wait_until("waiter registered", || {
        harness.manager.waiter_key_count() == 1
    });

    handle.complete("done");

    let record = waiting.join().expect("join").expect("terminal record");
    assert_eq!(record.final_response.as_deref(), Some("done"));
    assert_eq!(harness.manager.waiter_key_count(), 0);
}

#[test]
fn given_pending_task_when_slot_promoted_then_deferred_child_listener_attaches_to_handle() {
    let runner = FakeRunner::new();
    let harness = make_manager(HarnessOptions {
        config: Some(config(1, 1)),
        in_process: Some(Arc::clone(&runner)),
        process: Some(Arc::clone(&runner)),
        ..HarnessOptions::default()
    });
    let first = started(harness.manager.start(&named("running")));
    let queued = started(harness.manager.start(&named("queued")));
    assert_eq!(queued.status, TaskStatus::Pending);

    let unsubscribe = harness
        .manager
        .subscribe_child(&queued.task_id, Arc::new(|_| {}));
    assert!(runner.handle(&queued.task_id).is_none());
    runner.wait_handle(&first.task_id).complete("done");
    let promoted = runner.wait_handle(&queued.task_id);
    wait_until("three subscriptions", || promoted.subscribe_count() == 3);

    // The third subscribe is the manager's deferred child listener: FakeHandle records the call
    // before the launch thread installs the returned detach closure. Waiting on the count alone
    // races that install, so `unsubscribe` can land in the manager's missing-slot branch and run
    // the detach on the launch thread after the assertion. Arm a per-handle detach watcher first,
    // then await the actual detach invocation with a bounded timeout.
    let detach = promoted.watch_detach();
    unsubscribe();
    detach.wait(WAIT);

    assert_eq!(promoted.unsubscribe_count(), 1);
}

#[test]
fn given_queued_task_at_residency_cap_when_cancelled_then_wait_settles_and_later_spawn_admitted() {
    let runner = FakeRunner::new();
    let harness = make_lifecycle_manager(
        Arc::clone(&runner) as Arc<dyn ManagedRunner>,
        config(1, 1),
        json!({ "residency_max_children": 2 }),
    );
    let _first = started(harness.manager.start(&named("running")));
    let queued = started(harness.manager.start(&ManagerStartSpec {
        run_in_background: true,
        ..named("queued")
    }));
    assert_eq!(queued.status, TaskStatus::Pending);
    let (manager, task_id) = (harness.manager.clone(), queued.task_id.clone());
    let waiting = std::thread::spawn(move || manager.wait_for(&task_id, None, None));
    wait_until("waiter registered", || {
        harness.manager.waiter_key_count() == 1
    });

    let cancelled = harness
        .manager
        .cancel_task(
            &queued.task_id,
            Some("queue no longer needed"),
            CancelOptions::default(),
        )
        .expect("cancel");

    let CancelOutcome::Cancelled {
        previous_status, ..
    } = cancelled
    else {
        panic!("expected cancelled, got {cancelled:?}");
    };
    assert_eq!(previous_status, TaskStatus::Pending);
    assert_eq!(
        waiting.join().expect("join").expect("record").status,
        TaskStatus::Cancelled
    );
    let record = harness
        .store
        .load(&queued.task_id)
        .expect("load")
        .expect("record");
    assert_eq!(record.status, TaskStatus::Cancelled);
    assert_eq!(record.residency_state, ResidencyState::Disposed);
    let listed = harness.manager.list(&ListScope::All);
    let entry = listed
        .iter()
        .find(|entry| entry.record.task_id == queued.task_id)
        .expect("listed");
    assert_eq!(entry.queue_position, None);
    let later = started(harness.manager.start(&named("later")));
    assert_eq!(later.status, TaskStatus::Pending);
    assert_eq!(runner.started_count(), 1);
}

/// TS wraps `store.transition` to cancel inside the grant's start; the concrete Rust store has no
/// such seam, so the queued record is cancelled just before its slot transfers, which yields the
/// same losing start transition.
#[test]
fn given_grant_whose_start_transition_loses_to_cancel_when_slot_transfers_then_runner_skipped() {
    let runner = FakeRunner::new();
    let harness = make_manager(HarnessOptions {
        config: Some(config(1, 1)),
        in_process: Some(Arc::clone(&runner)),
        process: Some(Arc::clone(&runner)),
        ..HarnessOptions::default()
    });
    let first = started(harness.manager.start(&named("first")));
    let queued = started(harness.manager.start(&named("queued")));
    let transitioned = harness
        .store
        .transition(
            &queued.task_id,
            &crate::state::TaskTransition::Cancel {
                timestamp: chrono::Utc::now().to_rfc3339(),
                error_message: None,
                run_stats: None,
            },
        )
        .expect("cancel transition");
    assert!(transitioned.applied);

    runner.wait_handle(&first.task_id).complete("done");
    wait_terminal(&harness.manager, &first.task_id);
    wait_until("queued grant consumed", || {
        harness.manager.released_key_count() == 2
    });

    assert_eq!(
        status_of(&harness.store, &queued.task_id),
        Some(TaskStatus::Cancelled)
    );
    assert_eq!(runner.started_count(), 1);
    let later = started(harness.manager.start(&named("later")));
    assert_eq!(later.status, TaskStatus::Running);
    assert_eq!(runner.started_count(), 2);
}

struct GatedRunner {
    first: Mutex<Option<Arc<FakeHandle>>>,
    delayed: Mutex<Option<Arc<FakeHandle>>>,
    second_started: Mutex<bool>,
    release_second: Mutex<bool>,
    calls: Mutex<usize>,
}

impl ManagedRunner for GatedRunner {
    fn start(&self, spec: &ManagedStartSpec) -> ManagedRunnerResult {
        let call = {
            let mut calls = lock(&self.calls);
            *calls += 1;
            *calls
        };
        let handle = FakeHandle::new(&spec.task_id, None);
        match call {
            1 => *lock(&self.first) = Some(Arc::clone(&handle)),
            2 => {
                *lock(&self.delayed) = Some(Arc::clone(&handle));
                *lock(&self.second_started) = true;
                notify();
                wait_until("second start released", || *lock(&self.release_second));
            }
            _ => {}
        }
        notify();
        Ok(handle)
    }
}

#[test]
fn given_cancel_lands_while_runner_start_awaits_when_handle_arrives_then_destroyed_and_slot_released()
 {
    let runner = Arc::new(GatedRunner {
        first: Mutex::new(None),
        delayed: Mutex::new(None),
        second_started: Mutex::new(false),
        release_second: Mutex::new(false),
        calls: Mutex::new(0),
    });
    let harness = make_lifecycle_manager(
        Arc::clone(&runner) as Arc<dyn ManagedRunner>,
        config(1, 1),
        json!({}),
    );
    let _first = started(harness.manager.start(&named("first")));
    let queued = started(harness.manager.start(&named("queued")));
    let (manager, task_id) = (harness.manager.clone(), queued.task_id.clone());
    let waiting = std::thread::spawn(move || manager.wait_for(&task_id, None, None));
    wait_until("waiter registered", || {
        harness.manager.waiter_key_count() == 1
    });
    lock(&runner.first)
        .clone()
        .expect("first handle")
        .complete("done");
    wait_until("second start", || *lock(&runner.second_started));

    let cancelled = harness
        .manager
        .cancel_task(
            &queued.task_id,
            Some("cancel during start"),
            CancelOptions::default(),
        )
        .expect("cancel");
    *lock(&runner.release_second) = true;
    notify();
    let delayed = {
        wait_until("delayed handle", || lock(&runner.delayed).is_some());
        lock(&runner.delayed).clone().expect("delayed")
    };
    wait_until("delayed handle disposed", || delayed.dispose_calls() == 1);

    assert!(matches!(cancelled, CancelOutcome::Cancelled { .. }));
    assert_eq!(delayed.abort_calls(), 1);
    assert_eq!(delayed.dispose_calls(), 1);
    assert!(
        !harness
            .manager
            .resident_task_ids()
            .contains(&queued.task_id)
    );
    assert_eq!(
        waiting.join().expect("join").expect("record").status,
        TaskStatus::Cancelled
    );
    let record = harness
        .store
        .load(&queued.task_id)
        .expect("load")
        .expect("record");
    assert_eq!(record.residency_state, ResidencyState::Disposed);
    wait_until("slot released", || {
        harness.manager.released_key_count() == 2
    });
    let later = started(harness.manager.start(&named("later")));
    assert_eq!(later.status, TaskStatus::Running);
}

#[test]
fn given_running_task_with_one_queued_when_running_cancelled_then_queued_receives_slot() {
    let harness = make_manager(HarnessOptions {
        config: Some(config(1, 1)),
        ..HarnessOptions::default()
    });
    let running = started(harness.manager.start(&named("running")));
    let queued = started(harness.manager.start(&named("queued")));

    let cancelled = harness
        .manager
        .cancel_task(&running.task_id, Some("stop"), CancelOptions::default())
        .expect("cancel");

    assert!(matches!(cancelled, CancelOutcome::Cancelled { .. }));
    assert_eq!(
        status_of(&harness.store, &running.task_id),
        Some(TaskStatus::Cancelled)
    );
    harness.in_process.wait_handle(&queued.task_id);
    wait_until("queued running", || {
        status_of(&harness.store, &queued.task_id) == Some(TaskStatus::Running)
    });
}
