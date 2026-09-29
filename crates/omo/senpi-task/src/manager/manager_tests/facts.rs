//! `manager/manager-helpers.test.ts`, `manager/manager-hygiene.test.ts`,
//! `manager/manager-status-facts.test.ts`.

use std::sync::Arc;

use serde_json::json;

use super::fakes::{
    base_spec, default_manager, named, started, status_of, wait_terminal, wait_until,
};
use crate::manager::continue_result::ContinueResult;
use crate::manager::execution_mode::ExecutionMode;
use crate::manager::helpers::build_record_input;
use crate::manager::types::{ManagerStartSpec, ResolvedChildPlan};
use crate::shared::ManagedChildEvent;
use crate::state::{DeliverAs, TaskRecordInput, TaskStatus, create_task_record};
use crate::store::parse_task_record;

fn plan() -> ResolvedChildPlan {
    ResolvedChildPlan {
        model: "anthropic/claude-opus-4".to_string(),
        ..ResolvedChildPlan::default()
    }
}

fn spawn_spec(task_summary: Option<&str>) -> ManagerStartSpec {
    ManagerStartSpec {
        prompt: "TASK: Audit the record plumbing.".to_string(),
        parent_session_id: "session-1".to_string(),
        depth: 1,
        category: Some("quick".to_string()),
        task_summary: task_summary.map(str::to_string),
        ..ManagerStartSpec::default()
    }
}

// manager-helpers.test.ts

#[test]
fn given_spawn_spec_with_task_summary_when_record_input_built_then_summary_carried() {
    let input = build_record_input(
        &spawn_spec(Some("Audit the record plumbing")),
        &plan(),
        "auditor",
        ExecutionMode::InProcess,
    );

    assert_eq!(
        input.task_summary.as_deref(),
        Some("Audit the record plumbing")
    );
}

#[test]
fn given_no_task_summary_when_record_input_built_then_field_absent() {
    let input = build_record_input(
        &spawn_spec(None),
        &plan(),
        "auditor",
        ExecutionMode::InProcess,
    );

    assert_eq!(input.task_summary, None);
}

#[test]
fn given_record_with_task_summary_when_created_and_reparsed_then_summary_survives() {
    let record = create_task_record(
        TaskRecordInput {
            task_summary: Some("Audit the record plumbing".to_string()),
            parent_session_id: "session-1".to_string(),
            root_session_id: "session-1".to_string(),
            depth: 1,
            execution_mode: "in-process".to_string(),
            model: "anthropic/claude-opus-4".to_string(),
            notify_on_terminal: false,
            ..TaskRecordInput::default()
        },
        None,
    )
    .expect("record");
    let value = serde_json::to_value(&record).expect("serialize");

    let reparsed = parse_task_record(&value, "record.json", &mut Vec::new()).expect("parse");

    assert_eq!(
        reparsed.task_summary.as_deref(),
        Some("Audit the record plumbing")
    );
}

// manager-hygiene.test.ts

#[test]
fn given_task_revived_multiple_times_when_each_run_completes_then_release_guard_keeps_one_entry() {
    let harness = default_manager();
    let task = started(harness.manager.start(&named("a")));
    let fake = harness.in_process.wait_handle(&task.task_id);

    for cycle in 0..3 {
        fake.complete(&format!("pass-{cycle}"));
        wait_until("run released", || {
            harness.manager.released_key_count() == 1
                && status_of(&harness.store, &task.task_id) == Some(TaskStatus::Completed)
        });
        if cycle < 2 {
            let revived = harness
                .manager
                .continue_task(&task.task_id, "again", Some(DeliverAs::FollowUp))
                .expect("continue");
            assert!(
                matches!(revived, ContinueResult::Continued { .. }),
                "expected continued, got {revived:?}"
            );
        }
    }

    assert_eq!(harness.manager.released_key_count(), 1);
}

#[test]
fn given_completed_task_when_forgotten_then_release_guard_entry_pruned() {
    let harness = default_manager();
    let task = started(harness.manager.start(&named("a")));
    harness
        .in_process
        .wait_handle(&task.task_id)
        .complete("done");
    wait_terminal(&harness.manager, &task.task_id);
    wait_until("run released", || harness.manager.released_key_count() == 1);

    harness.manager.forget(&task.task_id);

    assert_eq!(harness.manager.released_key_count(), 0);
}

#[test]
fn given_running_task_with_transcript_subscription_when_forgotten_then_subscription_torn_down() {
    let harness = default_manager();
    let task = started(harness.manager.start(&named("a")));
    let fake = harness.in_process.wait_handle(&task.task_id);
    wait_until("owned subscriptions", || fake.subscribe_count() == 2);
    assert_eq!(fake.unsubscribe_count(), 0);

    harness.manager.forget(&task.task_id);

    assert_eq!(fake.unsubscribe_count(), fake.subscribe_count());
}

// manager-status-facts.test.ts

#[test]
fn given_spec_with_description_when_task_starts_then_persisted_record_keeps_it() {
    let harness = default_manager();

    let task = started(harness.manager.start(&ManagerStartSpec {
        description: Some("Audit the waiting-line renderers".to_string()),
        ..base_spec()
    }));

    let record = harness
        .store
        .load(&task.task_id)
        .expect("load")
        .expect("record");
    assert_eq!(
        record.description.as_deref(),
        Some("Audit the waiting-line renderers")
    );
}

#[test]
fn given_live_child_mid_run_when_run_stats_read_then_in_flight_accumulator_exposed() {
    let harness = default_manager();
    let task = started(harness.manager.start(&base_spec()));
    let fake = harness.in_process.wait_handle(&task.task_id);
    wait_until("owned subscriptions", || fake.subscribe_count() == 2);
    fake.emit(&ManagedChildEvent {
        message: Some(json!({
            "role": "assistant",
            "content": [{ "type": "text", "text": "working" }],
            "usage": { "output": 120, "totalTokens": 300 },
        })),
        ..ManagedChildEvent::of("message_end")
    });
    fake.emit(&ManagedChildEvent {
        tool_name: Some("read".to_string()),
        args: Some(json!({ "path": "a.ts" })),
        ..ManagedChildEvent::of("tool_execution_start")
    });

    let live = harness
        .manager
        .run_stats_snapshot(&task.task_id)
        .expect("live stats");

    assert_eq!(live.turns, 1);
    assert_eq!(live.tool_calls, 1);
    assert_eq!(live.output_tokens, Some(120));
}

#[test]
fn given_unknown_task_id_when_run_stats_read_then_nothing_reported() {
    let harness = default_manager();

    assert_eq!(harness.manager.run_stats_snapshot("st_missing"), None);
}

#[test]
fn given_listener_registered_before_promotion_when_cleanup_runs_then_handle_unsubscribes_at_most_once()
 {
    let harness = default_manager();
    let task = started(harness.manager.start(&base_spec()));
    let fake = harness.in_process.wait_handle(&task.task_id);
    let cleanup = harness
        .manager
        .subscribe_child(&task.task_id, Arc::new(|_event: &ManagedChildEvent| {}));

    // `Unsubscribe` is `FnOnce`: the TS double-cleanup is unrepresentable, so one call suffices.
    cleanup();
    fake.complete("done");
    wait_terminal(&harness.manager, &task.task_id);

    assert!(fake.unsubscribe_count() <= 1);
}
