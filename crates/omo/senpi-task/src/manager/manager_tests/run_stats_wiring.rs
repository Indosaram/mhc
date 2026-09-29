//! `manager/run-stats-wiring.test.ts`.

use serde_json::{Value, json};

use super::fakes::{Harness, base_spec, default_manager, started, wait_terminal, wait_until};
use crate::runners::{RunnerFailureKind, RunnerOutcome};
use crate::shared::ManagedChildEvent;
use crate::state::{TaskRunStats, TaskStatus};
use crate::steering::{CancelOptions, CancelOutcome, SendInput, SendOutcome};

const EPSILON: f64 = 1e-10;

fn message_end(text: &str, usage: Value) -> ManagedChildEvent {
    ManagedChildEvent {
        event_type: "message_end".to_string(),
        message: Some(json!({
            "role": "assistant",
            "content": [{ "type": "text", "text": text }],
            "usage": usage,
        })),
        ..ManagedChildEvent::default()
    }
}

fn tool_start(tool_name: &str, args: Value) -> ManagedChildEvent {
    ManagedChildEvent {
        event_type: "tool_execution_start".to_string(),
        tool_name: Some(tool_name.to_string()),
        args: Some(args),
        ..ManagedChildEvent::default()
    }
}

fn stored_stats(harness: &Harness, task_id: &str) -> Option<TaskRunStats> {
    harness
        .store
        .load(task_id)
        .expect("load")
        .and_then(|record| record.run_stats)
}

fn close(actual: Option<f64>, expected: f64) {
    let actual = actual.expect("value present");
    assert!(
        (actual - expected).abs() < EPSILON,
        "{actual} != {expected}"
    );
}

fn launch(harness: &Harness) -> (String, std::sync::Arc<super::fakes::FakeHandle>) {
    let task = started(harness.manager.start(&base_spec()));
    let fake = harness.in_process.wait_handle(&task.task_id);
    wait_until("manager subscribed", || fake.subscribe_count() > 0);
    (task.task_id, fake)
}

#[test]
fn given_spawned_child_when_completes_then_terminal_record_carries_run_stats() {
    let harness = default_manager();
    let (task_id, fake) = launch(&harness);

    fake.emit(&message_end(
        "working",
        json!({ "output": 120, "totalTokens": 300 }),
    ));
    fake.emit(&tool_start("read", json!({ "path": "a.ts" })));
    fake.complete("done");
    let terminal = wait_terminal(&harness.manager, &task_id);

    assert_eq!(terminal.status, TaskStatus::Completed);
    let stats = stored_stats(&harness, &task_id).expect("run stats");
    assert_eq!(stats.turns, 1);
    assert_eq!(stats.tool_calls, 1);
    assert_eq!(stats.output_tokens, Some(120));
    assert_eq!(stats.total_tokens, Some(300));
}

#[test]
fn given_child_reporting_cost_and_cache_usage_when_completes_then_cost_and_cache_hit_rate_persist()
{
    let harness = default_manager();
    let (task_id, fake) = launch(&harness);

    fake.emit(&message_end(
        "working",
        json!({
            "input": 100, "output": 120, "cacheRead": 300, "cacheWrite": 100, "totalTokens": 620,
            "cost": { "input": 0.02, "output": 0.1, "total": 0.12 },
        }),
    ));
    fake.complete("done");
    wait_terminal(&harness.manager, &task_id);

    let stats = stored_stats(&harness, &task_id).expect("run stats");
    close(stats.cost_usd, 0.12);
    close(stats.cache_hit_rate_last, 0.6);
    close(stats.cache_hit_rate_run, 0.6);
}

#[test]
fn given_live_child_with_cost_usage_when_snapshot_read_mid_run_then_cost_and_cache_visible() {
    let harness = default_manager();
    let (task_id, fake) = launch(&harness);

    fake.emit(&message_end(
        "partial",
        json!({ "input": 50, "output": 10, "cacheRead": 50, "cacheWrite": 0, "totalTokens": 110, "cost": 0.0025 }),
    ));
    fake.emit(&message_end(
        "hot",
        json!({ "input": 10, "output": 10, "cacheRead": 90, "cacheWrite": 0, "totalTokens": 110, "cost": 0.005 }),
    ));

    let live = harness
        .manager
        .run_stats_snapshot(&task_id)
        .expect("live snapshot");
    close(live.cost_usd, 0.0075);
    close(live.cache_hit_rate_last, 0.9);
    close(live.cache_hit_rate_run, 140.0 / 200.0);
}

#[test]
fn given_spawned_child_when_fails_then_run_stats_persist_on_error_record() {
    let harness = default_manager();
    let (task_id, fake) = launch(&harness);

    fake.emit(&tool_start("bash", json!({ "command": "ls" })));
    fake.fail(RunnerFailureKind::ChildTurnFailed, "boom");
    let terminal = wait_terminal(&harness.manager, &task_id);

    assert_eq!(terminal.status, TaskStatus::Error);
    assert_eq!(
        stored_stats(&harness, &task_id).map(|stats| stats.tool_calls),
        Some(1)
    );
}

#[test]
fn given_spawned_child_when_cancelled_then_run_stats_persist_on_cancelled_record() {
    let harness = default_manager();
    let (task_id, fake) = launch(&harness);

    fake.emit(&message_end(
        "partial",
        json!({ "output": 10, "totalTokens": 20 }),
    ));
    fake.settle(RunnerOutcome::Cancelled);
    let terminal = wait_terminal(&harness.manager, &task_id);

    assert_eq!(terminal.status, TaskStatus::Cancelled);
    assert_eq!(
        stored_stats(&harness, &task_id).map(|stats| stats.turns),
        Some(1)
    );
}

#[test]
fn given_running_child_with_usage_when_cancel_task_then_cancelled_record_carries_run_stats() {
    let harness = default_manager();
    let (task_id, fake) = launch(&harness);
    fake.emit(&message_end(
        "working",
        json!({ "output": 77, "totalTokens": 300 }),
    ));
    fake.emit(&tool_start("read", json!({})));

    let outcome = harness
        .manager
        .cancel_task(&task_id, Some("user cancelled"), CancelOptions::default())
        .expect("cancel");

    assert!(
        matches!(outcome, CancelOutcome::Cancelled { .. }),
        "{outcome:?}"
    );
    let record = harness.store.load(&task_id).expect("load").expect("record");
    assert_eq!(record.status, TaskStatus::Cancelled);
    let stats = record.run_stats.expect("run stats");
    assert_eq!(stats.turns, 1);
    assert_eq!(stats.tool_calls, 1);
    assert_eq!(stats.output_tokens, Some(77));
}

#[test]
fn given_completed_run_revived_then_cancelled_when_revived_run_has_no_events_then_no_stale_stats() {
    let harness = default_manager();
    let (task_id, fake) = launch(&harness);
    fake.emit(&message_end(
        "one",
        json!({ "output": 50, "totalTokens": 100 }),
    ));
    fake.complete("done");
    wait_terminal(&harness.manager, &task_id);
    assert_eq!(
        stored_stats(&harness, &task_id).and_then(|stats| stats.output_tokens),
        Some(50)
    );

    let send = harness
        .manager
        .send_to_task(&SendInput::new(task_id.clone(), "keep going"))
        .expect("send");
    assert!(matches!(send, SendOutcome::Revived { .. }), "{send:?}");
    let outcome = harness
        .manager
        .cancel_task(
            &task_id,
            Some("stop the revived run"),
            CancelOptions::default(),
        )
        .expect("cancel");

    assert!(
        matches!(outcome, CancelOutcome::Cancelled { .. }),
        "{outcome:?}"
    );
    let stats = stored_stats(&harness, &task_id).expect("run stats");
    assert_eq!(stats.output_tokens, None);
    assert_eq!(stats.turns, 0);
}
