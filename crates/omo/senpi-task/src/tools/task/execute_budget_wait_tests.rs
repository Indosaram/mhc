//! `tools/task/execute-budget-wait.test.ts`
//!
//! The TS suite drives `buildTaskExecute` with a fake manager and a scripted deadline scheduler.
//! In Rust the foreground wait is synchronous and bounded by `ForegroundWaitOptions::deadline`
//! (the `scheduleDeadline` seam), so these cases exercise `wait_for_foreground_task` directly
//! against a scripted real `TaskManager`: a zero deadline plays the role of "the deadline fired",
//! and a completed child before the wait plays the role of "the wait resolved first".

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use pretty_assertions::assert_eq;

use crate::manager::manager_tests::fakes::{base_spec, default_manager, started, wait_terminal};
use crate::tools::task::foreground_wait::{
    ForegroundWaitInput, ForegroundWaitOptions, ForegroundWaitResult, PROMPT_CACHE_SAFE_WAIT_ENV,
    PromptCacheSafeWaitSeconds, TaskToolContext, resolve_prompt_cache_safe_wait_seconds,
    wait_for_foreground_task,
};

fn ctx() -> TaskToolContext {
    TaskToolContext {
        cwd: "/tmp/project".to_string(),
        session_id: "parent-1".to_string(),
        get_prompt_cache_safe_wait_seconds: None,
    }
}

fn context_with_budget(get_budget: PromptCacheSafeWaitSeconds) -> TaskToolContext {
    TaskToolContext {
        get_prompt_cache_safe_wait_seconds: Some(get_budget),
        ..ctx()
    }
}

fn env_with(value: &str) -> HashMap<String, String> {
    HashMap::from([(PROMPT_CACHE_SAFE_WAIT_ENV.to_string(), value.to_string())])
}

/// Options whose deadline has effectively already fired.
fn fired_deadline() -> ForegroundWaitOptions {
    ForegroundWaitOptions {
        env: Some(HashMap::new()),
        deadline: Some(Duration::ZERO),
    }
}

/// Options whose deadline is far enough away that an already-settled child wins.
fn pending_deadline() -> ForegroundWaitOptions {
    ForegroundWaitOptions {
        env: Some(HashMap::new()),
        deadline: Some(Duration::from_secs(30)),
    }
}

#[test]
fn given_a_context_getter_and_env_bridge_when_resolving_the_budget_then_the_feature_detected_getter_wins()
 {
    // given
    let budgeted = context_with_budget(Arc::new(|| Some(270)));

    // when
    let from_getter = resolve_prompt_cache_safe_wait_seconds(&budgeted, Some(&env_with("12")));
    let from_env = resolve_prompt_cache_safe_wait_seconds(&ctx(), Some(&env_with("45")));
    let absent = resolve_prompt_cache_safe_wait_seconds(&ctx(), Some(&HashMap::new()));

    // then
    assert_eq!(from_getter, Some(270));
    assert_eq!(from_env, Some(45));
    assert_eq!(absent, None);
}

#[test]
fn given_a_getter_returning_undefined_when_resolving_the_budget_then_the_env_bridge_is_not_consulted()
 {
    // given
    let budgeted = context_with_budget(Arc::new(|| None));

    // when
    let resolved = resolve_prompt_cache_safe_wait_seconds(&budgeted, Some(&env_with("45")));

    // then
    assert_eq!(resolved, None);
}

#[test]
fn given_a_running_foreground_child_when_its_prompt_cache_safe_budget_expires_then_it_is_promoted_without_cancellation()
 {
    // given
    let harness = default_manager();
    let task = started(harness.manager.start(&base_spec()));
    let budgeted = context_with_budget(Arc::new(|| Some(270)));

    // when
    let result = wait_for_foreground_task(&ForegroundWaitInput {
        manager: &harness.manager,
        task_id: &task.task_id,
        signal: None,
        ctx: &budgeted,
        options: fired_deadline(),
    })
    .expect("budget expiry converts instead of failing");

    // then
    match result {
        ForegroundWaitResult::Promoted { budget_seconds } => assert_eq!(budget_seconds, 270),
        ForegroundWaitResult::Completed { .. } => panic!("expected promotion to background"),
    }
    assert!(harness.manager.was_background(&task.task_id));
    let live = harness.manager.get(&task.task_id).expect("record exists");
    assert!(live.status.as_str() != "cancelled");

    harness
        .in_process
        .wait_handle(&task.task_id)
        .complete("done later");
    let settled = wait_terminal(&harness.manager, &task.task_id);
    assert_eq!(settled.status.as_str(), "completed");
}

#[test]
fn given_no_getter_and_no_env_budget_when_a_foreground_child_waits_then_no_deadline_applies_and_the_wait_is_unbounded()
 {
    // given
    let harness = default_manager();
    let task = started(harness.manager.start(&base_spec()));
    harness
        .in_process
        .wait_handle(&task.task_id)
        .complete("finished");

    // when: even an already-fired deadline is ignored without a budget
    let result = wait_for_foreground_task(&ForegroundWaitInput {
        manager: &harness.manager,
        task_id: &task.task_id,
        signal: None,
        ctx: &ctx(),
        options: fired_deadline(),
    })
    .expect("unbounded wait settles");

    // then
    match result {
        ForegroundWaitResult::Completed { record } => {
            assert_eq!(record.status.as_str(), "completed");
        }
        ForegroundWaitResult::Promoted { .. } => panic!("expected an inline completion"),
    }
    assert!(!harness.manager.was_background(&task.task_id));
}

#[test]
fn given_a_budgeted_foreground_child_when_it_settles_before_the_deadline_then_it_stays_inline() {
    // given
    let harness = default_manager();
    let task = started(harness.manager.start(&base_spec()));
    let budgeted = context_with_budget(Arc::new(|| Some(270)));
    harness.in_process.wait_handle(&task.task_id).complete("done");

    // when
    let result = wait_for_foreground_task(&ForegroundWaitInput {
        manager: &harness.manager,
        task_id: &task.task_id,
        signal: None,
        ctx: &budgeted,
        options: pending_deadline(),
    })
    .expect("settled child resolves");

    // then
    match result {
        ForegroundWaitResult::Completed { record } => {
            assert_eq!(record.status.as_str(), "completed");
        }
        ForegroundWaitResult::Promoted { .. } => panic!("expected an inline completion"),
    }
    assert!(!harness.manager.was_background(&task.task_id));
}

#[test]
fn given_a_foreground_batch_with_changing_budgets_when_waits_start_per_item_then_settled_items_stay_inline_and_each_remaining_item_converts_on_its_own_budget()
 {
    // given
    let budgets = [1_i64, 2, 3];
    let budget_reads = Arc::new(AtomicUsize::new(0));
    let reads = Arc::clone(&budget_reads);
    let budgeted = context_with_budget(Arc::new(move || {
        let index = reads.fetch_add(1, Ordering::SeqCst);
        budgets.get(index).copied()
    }));
    let harnesses = [default_manager(), default_manager(), default_manager()];
    let task_ids: Vec<String> = harnesses
        .iter()
        .map(|harness| started(harness.manager.start(&base_spec())).task_id)
        .collect();
    harnesses[0]
        .in_process
        .wait_handle(&task_ids[0])
        .complete("first done");

    // when
    let outcomes: Vec<ForegroundWaitResult> = harnesses
        .iter()
        .zip(&task_ids)
        .enumerate()
        .map(|(index, (harness, task_id))| {
            let options = if index == 0 {
                pending_deadline()
            } else {
                fired_deadline()
            };
            wait_for_foreground_task(&ForegroundWaitInput {
                manager: &harness.manager,
                task_id,
                signal: None,
                ctx: &budgeted,
                options,
            })
            .expect("batch item wait resolves")
        })
        .collect();

    // then
    assert_eq!(budget_reads.load(Ordering::SeqCst), 3);
    match &outcomes[0] {
        ForegroundWaitResult::Completed { record } => {
            assert_eq!(record.status.as_str(), "completed");
        }
        ForegroundWaitResult::Promoted { .. } => panic!("first item should stay inline"),
    }
    let promoted: Vec<i64> = outcomes[1..]
        .iter()
        .map(|outcome| match outcome {
            ForegroundWaitResult::Promoted { budget_seconds } => *budget_seconds,
            ForegroundWaitResult::Completed { .. } => panic!("remaining items should convert"),
        })
        .collect();
    assert_eq!(promoted, vec![2, 3]);
    let background: Vec<bool> = harnesses
        .iter()
        .zip(&task_ids)
        .map(|(harness, task_id)| harness.manager.was_background(task_id))
        .collect();
    assert_eq!(background, vec![false, true, true]);

    for (harness, task_id) in harnesses.iter().zip(&task_ids).skip(1) {
        harness.in_process.wait_handle(task_id).complete("done");
        let settled = wait_terminal(&harness.manager, task_id);
        assert_eq!(settled.status.as_str(), "completed");
    }
}
