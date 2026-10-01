//! `tools/task/execute-batch.test.ts`
//!
//! The Rust `TaskManager` is concrete, so the TS `createFakeManager` script is modelled by driving
//! `execute_batch` directly: the `start_item` callback plays the scripted `start`, starting real
//! tasks on the fake-runner manager (and completing them through the in-process fake handle) or
//! returning scripted non-started outcomes.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use pretty_assertions::assert_eq;

use crate::manager::manager_tests::fakes::{base_spec, default_manager};
use crate::manager::types::{
    ManagerStartSpec, PlanResolutionCode, PlanResolutionError, StartResult,
};
use crate::manager::{AbortSignal, TaskManager};
use crate::tools::task::execute_batch::{
    ExecuteBatchInput, StartItem, TaskToolResult, execute_batch,
};
use crate::tools::task::foreground_wait::{ForegroundWaitOptions, TaskToolContext};
use crate::tools::task::params::MAX_TASK_BATCH_ITEMS;
use crate::tools::task::types::{ResolvedSpawnItem, TaskToolMode};
use crate::tools::task::validation::{SpawnParamsInput, resolve_spawn_items};

fn ctx() -> TaskToolContext {
    TaskToolContext {
        session_id: "parent-1".to_string(),
        ..Default::default()
    }
}

fn items(count: usize) -> Vec<ResolvedSpawnItem> {
    let params = SpawnParamsInput {
        prompt: Some("one".to_string()),
        category: Some("quick".to_string()),
        ..Default::default()
    };
    let Ok(resolved) = resolve_spawn_items(&params) else {
        panic!("expected the legacy prompt to resolve");
    };
    let item = resolved.into_iter().next().expect("one resolved item");
    vec![item; count]
}

fn run(
    manager: &TaskManager,
    items: &[ResolvedSpawnItem],
    signal: Option<&AbortSignal>,
    run_in_background: bool,
    start_item: &StartItem,
) -> TaskToolResult {
    let ctx = ctx();
    execute_batch(&ExecuteBatchInput {
        manager,
        items,
        signal,
        ctx: &ctx,
        run_in_background,
        start_item,
        skill_summary_for: None,
        options: ForegroundWaitOptions::default(),
    })
}

fn start_named(manager: &TaskManager, index: usize) -> StartResult {
    manager.start(&ManagerStartSpec {
        name: Some(format!("item-{index}")),
        ..base_spec()
    })
}

fn started_id(start: &StartResult) -> Option<String> {
    match start {
        StartResult::Started(started) => Some(started.task_id.to_string()),
        _ => None,
    }
}

fn plan_unresolved(
    code: PlanResolutionCode,
    message: &str,
    available_categories: Option<Vec<String>>,
) -> StartResult {
    StartResult::PlanUnresolved(PlanResolutionError {
        code,
        message: message.to_string(),
        available_categories,
        available_agents: None,
        attempted_chain: None,
        category: None,
        missing_providers: None,
    })
}

#[test]
fn w2batch_given_three_sync_items_when_all_complete_then_details_preserve_input_order_with_aggregate_completed()
 {
    // given
    let harness = &*Box::leak(Box::new(default_manager()));
    let ids: Arc<Mutex<Vec<String>>> = Arc::default();
    let recorded = Arc::clone(&ids);
    let counter = Arc::new(AtomicUsize::new(0));
    let start_item = move |_item: &ResolvedSpawnItem| -> Result<StartResult, String> {
        let index = counter.fetch_add(1, Ordering::SeqCst) + 1;
        let start = start_named(&harness.manager, index);
        if let Some(id) = started_id(&start) {
            let text = format!("done:{id}");
            harness.in_process.wait_handle(&id).complete(&text);
            recorded.lock().expect("ids lock").push(id);
        }
        Ok(start)
    };

    // when
    let output = run(&harness.manager, &items(3), None, false, &start_item);

    // then
    let ids = ids.lock().expect("ids lock").clone();
    assert_eq!(ids.len(), 3);
    assert_eq!(output.details.status, "completed");
    let details = output.details.items.expect("batch items");
    assert_eq!(
        details
            .iter()
            .map(|item| (item.task_id.clone(), item.status.clone()))
            .collect::<Vec<_>>(),
        ids.iter()
            .map(|id| (id.clone(), "completed".to_string()))
            .collect::<Vec<_>>()
    );
    let text = output.content[0].text.clone();
    assert!(text.starts_with("Batch completed."));
    for id in &ids {
        assert!(text.contains(&format!("done:{id}")));
    }
}

#[test]
fn w2batch_given_one_failed_item_when_the_sync_batch_settles_then_both_successes_remain_and_aggregate_status_is_error()
 {
    // given
    let harness = &*Box::leak(Box::new(default_manager()));
    let ids: Arc<Mutex<Vec<String>>> = Arc::default();
    let recorded = Arc::clone(&ids);
    let counter = Arc::new(AtomicUsize::new(0));
    let start_item = move |_item: &ResolvedSpawnItem| -> Result<StartResult, String> {
        let index = counter.fetch_add(1, Ordering::SeqCst);
        if index == 1 {
            return Ok(plan_unresolved(
                PlanResolutionCode::InvalidTarget,
                "runner exploded",
                None,
            ));
        }
        let start = start_named(&harness.manager, index + 1);
        if let Some(id) = started_id(&start) {
            let text = format!("done:{id}");
            harness.in_process.wait_handle(&id).complete(&text);
            recorded.lock().expect("ids lock").push(id);
        }
        Ok(start)
    };

    // when
    let output = run(&harness.manager, &items(3), None, false, &start_item);

    // then
    let ids = ids.lock().expect("ids lock").clone();
    assert_eq!(ids.len(), 2);
    assert_eq!(output.details.status, "error");
    let details = output.details.items.expect("batch items");
    assert_eq!(details.len(), 3);
    assert_eq!(details[0].task_id, ids[0]);
    assert_eq!(details[0].status, "completed");
    assert_eq!(details[1].task_id, "");
    assert_eq!(details[1].status, "error");
    assert_eq!(details[1].error_message.as_deref(), Some("runner exploded"));
    assert_eq!(details[2].task_id, ids[1]);
    assert_eq!(details[2].status, "completed");
}

#[test]
fn w2batch_given_an_oversized_batch_when_executed_then_it_rejects_before_starting_any_item() {
    // given
    let harness = default_manager();
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&calls);
    let start_item = move |_item: &ResolvedSpawnItem| -> Result<StartResult, String> {
        counted.fetch_add(1, Ordering::SeqCst);
        Err("batch start must not run".to_string())
    };

    // when
    let output = run(
        &harness.manager,
        &items(MAX_TASK_BATCH_ITEMS + 1),
        None,
        false,
        &start_item,
    );

    // then
    let reason = format!("tasks supports at most {MAX_TASK_BATCH_ITEMS} items.");
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(output.details.task_id, "");
    assert_eq!(output.details.status, "invalid_arguments");
    assert_eq!(output.details.mode, TaskToolMode::Spawn);
    assert_eq!(output.details.reason.as_deref(), Some(reason.as_str()));
    assert_eq!(output.content[0].text, reason);
}

#[test]
fn w2batch_given_a_thrown_middle_start_when_later_items_can_start_then_every_item_outcome_is_preserved()
 {
    // given
    let harness = &*Box::leak(Box::new(default_manager()));
    let ids: Arc<Mutex<Vec<String>>> = Arc::default();
    let recorded = Arc::clone(&ids);
    let counter = Arc::new(AtomicUsize::new(0));
    let observed = Arc::clone(&counter);
    let start_item = move |_item: &ResolvedSpawnItem| -> Result<StartResult, String> {
        let index = counter.fetch_add(1, Ordering::SeqCst);
        if index == 1 {
            return Err("middle start exploded".to_string());
        }
        let start = start_named(&harness.manager, index + 1);
        if let Some(id) = started_id(&start) {
            let text = format!("done:{id}");
            harness.in_process.wait_handle(&id).complete(&text);
            recorded.lock().expect("ids lock").push(id);
        }
        Ok(start)
    };

    // when
    let output = run(&harness.manager, &items(3), None, false, &start_item);

    // then
    let ids = ids.lock().expect("ids lock").clone();
    assert_eq!(observed.load(Ordering::SeqCst), 3);
    assert_eq!(output.details.status, "error");
    let details = output.details.items.expect("batch items");
    assert_eq!(details.len(), 3);
    assert_eq!(details[0].task_id, ids[0]);
    assert_eq!(details[0].status, "completed");
    assert_eq!(details[1].task_id, "");
    assert_eq!(details[1].status, "error");
    assert_eq!(
        details[1].error_message.as_deref(),
        Some("middle start exploded")
    );
    assert_eq!(details[2].task_id, ids[1]);
    assert_eq!(details[2].status, "completed");
}

#[test]
fn w2batch_given_a_parent_abort_during_three_waits_when_the_batch_settles_then_every_non_terminal_child_is_cancelled()
 {
    // given
    let harness = &*Box::leak(Box::new(default_manager()));
    let signal: &'static AbortSignal = Box::leak(Box::new(AbortSignal::default()));
    let ids: Arc<Mutex<Vec<String>>> = Arc::default();
    let recorded = Arc::clone(&ids);
    let counter = Arc::new(AtomicUsize::new(0));
    let start_item = move |_item: &ResolvedSpawnItem| -> Result<StartResult, String> {
        let index = counter.fetch_add(1, Ordering::SeqCst);
        let start = start_named(&harness.manager, index + 1);
        if let Some(id) = started_id(&start) {
            recorded.lock().expect("ids lock").push(id);
        }
        if index == 2 {
            // The parent turn aborts once every child is running and awaited.
            signal.abort();
        }
        Ok(start)
    };

    // when
    let output = run(&harness.manager, &items(3), Some(signal), false, &start_item);

    // then
    let ids = ids.lock().expect("ids lock").clone();
    assert_eq!(ids.len(), 3);
    assert_eq!(output.details.status, "cancelled");
    let details = output.details.items.expect("batch items");
    assert_eq!(
        details
            .iter()
            .map(|item| item.status.clone())
            .collect::<Vec<_>>(),
        vec!["cancelled", "cancelled", "cancelled"]
    );
    assert_eq!(
        details
            .iter()
            .map(|item| item.task_id.clone())
            .collect::<Vec<_>>(),
        ids
    );
    for item in &details {
        assert_eq!(item.error_message.as_deref(), Some("parent turn aborted"));
    }
}

#[test]
fn w2batch_given_an_already_aborted_parent_when_executed_then_nothing_is_started() {
    // given
    let harness = default_manager();
    let signal = AbortSignal::default();
    signal.abort();
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&calls);
    let start_item = move |_item: &ResolvedSpawnItem| -> Result<StartResult, String> {
        counted.fetch_add(1, Ordering::SeqCst);
        Err("must not start".to_string())
    };

    // when
    let output = run(&harness.manager, &items(3), Some(&signal), false, &start_item);

    // then
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert_eq!(output.content[0].text, "Parent aborted before spawn");
    assert_eq!(output.details.task_id, "");
    assert_eq!(output.details.status, "cancelled");
    assert_eq!(output.details.mode, TaskToolMode::Spawn);
    assert_eq!(
        output.details.reason.as_deref(),
        Some("Parent aborted before spawn")
    );
}

#[test]
fn w2batch_given_one_denied_item_when_other_sync_items_complete_then_denial_has_an_empty_id_and_aggregate_error()
 {
    // given
    let harness = &*Box::leak(Box::new(default_manager()));
    let counter = Arc::new(AtomicUsize::new(0));
    let start_item = move |_item: &ResolvedSpawnItem| -> Result<StartResult, String> {
        let index = counter.fetch_add(1, Ordering::SeqCst);
        if index == 1 {
            return Ok(StartResult::DepthDenied {
                reason: "depth limit".to_string(),
                child_depth: 2,
                max_depth: 1,
            });
        }
        let start = start_named(&harness.manager, index + 1);
        if let Some(id) = started_id(&start) {
            harness.in_process.wait_handle(&id).complete("done");
        }
        Ok(start)
    };

    // when
    let output = run(&harness.manager, &items(3), None, false, &start_item);

    // then
    assert_eq!(output.details.status, "error");
    let details = output.details.items.expect("batch items");
    assert_eq!(details[1].task_id, "");
    assert_eq!(details[1].status, "error");
    assert_eq!(details[1].error_message.as_deref(), Some("depth limit"));
    assert_eq!(details[0].status, "completed");
    assert_eq!(details[2].status, "completed");
}

#[test]
fn w2batch_given_a_model_unavailable_start_failure_when_executed_then_the_error_names_valid_category_names_with_the_omo_json_config_hint()
 {
    // given
    let harness = default_manager();
    let start_item = |_item: &ResolvedSpawnItem| -> Result<StartResult, String> {
        Ok(plan_unresolved(
            PlanResolutionCode::ModelUnavailable,
            "No available model for category \"quick\" (attempted opengateway/glm-5.2-ultrafast).",
            Some(vec!["deep".to_string(), "quick".to_string()]),
        ))
    };

    // when
    let output = run(&harness.manager, &items(1), None, true, &start_item);

    // then
    let text = output.content[0].text.clone();
    assert!(text.contains("Valid category names: deep, quick"));
    assert!(text.contains("omo.json"));
    assert!(!text.contains("Pass model:"));
    assert!(!text.contains("Available categories:"));
    assert_eq!(output.details.status, "error");
}

#[test]
fn w2batch_given_an_invalid_target_start_failure_when_executed_then_the_error_lists_available_categories()
 {
    // given
    let harness = default_manager();
    let start_item = |_item: &ResolvedSpawnItem| -> Result<StartResult, String> {
        Ok(plan_unresolved(
            PlanResolutionCode::InvalidTarget,
            "Unknown category \"nope\".",
            Some(vec!["deep".to_string(), "quick".to_string()]),
        ))
    };

    // when
    let output = run(&harness.manager, &items(1), None, true, &start_item);

    // then
    let text = output.content[0].text.clone();
    assert!(text.contains("Unknown category \"nope\". Available categories: deep, quick."));
    assert!(!text.contains("Valid category names:"));
    let details = output.details.items.expect("batch items");
    assert_eq!(details[0].task_id, "");
    assert_eq!(details[0].status, "error");
}

#[test]
fn w2batch_given_background_items_when_all_start_then_the_batch_reports_running_with_continuation_footers()
 {
    // given
    let harness = &*Box::leak(Box::new(default_manager()));
    let ids: Arc<Mutex<Vec<String>>> = Arc::default();
    let recorded = Arc::clone(&ids);
    let counter = Arc::new(AtomicUsize::new(0));
    let start_item = move |_item: &ResolvedSpawnItem| -> Result<StartResult, String> {
        let index = counter.fetch_add(1, Ordering::SeqCst) + 1;
        let start = start_named(&harness.manager, index);
        if let Some(id) = started_id(&start) {
            recorded.lock().expect("ids lock").push(id);
        }
        Ok(start)
    };

    // when
    let output = run(&harness.manager, &items(2), None, true, &start_item);

    // then
    let ids = ids.lock().expect("ids lock").clone();
    assert_eq!(ids.len(), 2);
    assert_eq!(output.details.status, "running");
    assert_eq!(output.details.run_in_background, Some(true));
    assert_eq!(output.details.task_id, ids[0]);
    let text = output.content[0].text.clone();
    assert!(text.starts_with("Batch running."));
    for id in &ids {
        assert!(text.contains(&format!(
            "[task_id: {id} - continue with task_send(to=\"{id}\", message=\"...\")]"
        )));
    }
    let details = output.details.items.expect("batch items");
    assert_eq!(
        details
            .iter()
            .map(|item| item.task_id.clone())
            .collect::<Vec<_>>(),
        ids
    );
    for item in &details {
        assert!(item.status == "running" || item.status == "pending");
    }
}
