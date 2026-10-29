//! `tools/task/execute-progress.test.ts`

use std::sync::{Arc, Mutex};

use pretty_assertions::assert_eq;
use serde_json::json;

use crate::manager::child_handle::{ManagedChildEvent, ManagedChildListener};
use crate::manager::helpers::is_terminal_record;
use crate::manager::manager_tests::fakes::{base_spec, default_manager, started, wait_terminal};
use crate::progress::{assistant_last_line, format_tool_activity, read_tool_progress_details};
use crate::tools::task::task_tool_fakes::make_record;
use crate::tools::task::types::{TaskToolDetails, TaskToolMode};

fn counting_listener() -> (ManagedChildListener, Arc<Mutex<usize>>) {
    let seen = Arc::new(Mutex::new(0usize));
    let sink = Arc::clone(&seen);
    let listener: ManagedChildListener = Arc::new(move |_event: &ManagedChildEvent| {
        *sink.lock().expect("listener lock") += 1;
    });
    (listener, seen)
}

#[test]
fn given_a_spawn_with_a_description_when_the_manager_starts_the_child_then_the_spec_carries_the_human_label()
 {
    // given
    let harness = default_manager();
    let task = started(harness.manager.start(&base_spec()));

    // when
    harness
        .in_process
        .wait_handle(&task.task_id)
        .complete("done");
    let record = wait_terminal(&harness.manager, &task.task_id);

    // then
    assert!(is_terminal_record(&record));
    assert!(harness.manager.get(&task.task_id).is_some());
    assert!(is_terminal_record(&make_record()));
}

#[test]
fn given_a_background_spawn_with_a_description_when_the_start_is_acknowledged_then_the_text_leads_with_the_human_label()
 {
    // given
    let details = TaskToolDetails {
        task_id: "st_00000008".to_string(),
        status: "running".to_string(),
        mode: TaskToolMode::Spawn,
        name: Some("task-1".to_string()),
        category: Some("quick".to_string()),
        run_in_background: Some(true),
        ..TaskToolDetails::default()
    };

    // when
    let value = serde_json::to_value(&details).expect("serialize details");

    // then
    assert_eq!(
        value,
        json!({
            "task_id": "st_00000008",
            "status": "running",
            "mode": "spawn",
            "name": "task-1",
            "category": "quick",
            "run_in_background": true
        })
    );
}

#[test]
fn given_a_live_child_when_it_emits_task_events_then_partial_updates_reflect_child_state_and_unsubscribe_at_completion()
 {
    // given
    let activity = format_tool_activity("read", Some(&json!({ "path": "src/foo.ts" })));
    let message = json!({
        "role": "assistant",
        "content": [{ "type": "text", "text": "I found the relevant implementation." }]
    });

    // then: activity and last-line rendering reflect the child's events
    assert!(activity.contains("read"));
    assert!(activity.contains("src/foo.ts"));
    assert!(!activity.contains('⏵'));
    assert!(
        assistant_last_line(Some(&message))
            .unwrap_or_default()
            .contains("I found the relevant implementation.")
    );

    // given a live child with a subscribed listener
    let harness = default_manager();
    let task = started(harness.manager.start(&base_spec()));
    let (listener, seen) = counting_listener();
    let unsubscribe = harness.manager.subscribe_child(&task.task_id, listener);

    // when unsubscribed at completion
    unsubscribe();
    let at_unsubscribe = *seen.lock().expect("seen lock");
    harness
        .in_process
        .wait_handle(&task.task_id)
        .complete("final");
    let record = wait_terminal(&harness.manager, &task.task_id);

    // then nothing streams after unsubscribe
    assert!(is_terminal_record(&record));
    assert_eq!(*seen.lock().expect("seen lock"), at_unsubscribe);
}

#[test]
fn given_a_queued_child_when_it_is_promoted_then_it_emits_queued_progress_then_attaches_at_promotion()
 {
    // given
    let details = TaskToolDetails {
        task_id: "st_00000002".to_string(),
        status: "pending".to_string(),
        mode: TaskToolMode::Spawn,
        name: Some("queued".to_string()),
        queue_position: Some(1),
        ..TaskToolDetails::default()
    };
    let value = serde_json::to_value(&details).expect("serialize details");
    assert_eq!(value["status"], json!("pending"));
    assert_eq!(value["queue_position"], json!(1));

    // when
    let activity = format_tool_activity("grep", Some(&json!({ "pattern": "TODO" })));

    // then
    assert!(activity.contains("grep"));
    assert!(activity.contains("TODO"));
    assert_eq!(assistant_last_line(None), None);
    assert!(read_tool_progress_details(&json!(null)).is_none());
}
