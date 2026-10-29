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
    fake.wait_subscriptions(4);
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
    fake.wait_subscriptions(4);
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

#[test]
fn streamed_failed_tool_facts_survive_next_message_start_but_clear_after_parent_hop() {
    let harness = default_manager();
    let task = started(harness.manager.start(&base_spec()));
    harness
        .store
        .mutate(&task.task_id, |fresh: &crate::state::TaskRecord| {
            crate::state::TaskRecord {
                parent_model: Some("parent/model".into()),
                ..fresh.clone()
            }
        })
        .unwrap();
    let fake = harness.in_process.wait_handle(&task.task_id);
    fake.emit(&ManagedChildEvent {
        event_type: "message_update".into(),
        message: Some(json!({"role":"assistant","content":[{"type":"toolCall","name":"write"}]})),
        ..Default::default()
    });
    fake.emit(&ManagedChildEvent { event_type:"message_end".into(), message:Some(json!({"role":"assistant","content":[],"stopReason":"error","errorMessage":"weekly limit exhausted (resets 3:20am)"})), ..Default::default() });
    fake.emit(&ManagedChildEvent::of("message_start"));
    let event = ManagedChildEvent {
        event_type: "retry_fallback_applied".into(),
        from: Some("child/model:high".into()),
        to: Some("parent/model".into()),
        reason: Some("transient".into()),
        ..Default::default()
    };
    fake.emit(&event);
    let note = harness
        .store
        .load(&task.task_id)
        .unwrap()
        .unwrap()
        .parent_model_fallback
        .unwrap();
    assert_eq!(
        note.reason,
        crate::state::ParentModelFallbackReason::UsageLimit
    );
    assert_eq!(note.resets.as_deref(), Some("3:20am"));
    assert_eq!(note.from_model, "child/model");
    assert_eq!(note.interrupted_tools, Some(vec!["write".into()]));
    fake.emit(&event);
    let second = harness
        .store
        .load(&task.task_id)
        .unwrap()
        .unwrap()
        .parent_model_fallback
        .unwrap();
    assert_eq!(
        second.reason,
        crate::state::ParentModelFallbackReason::Unavailable
    );
    assert!(second.interrupted_tools.is_none());
    assert!(second.detail.is_none());
}

#[test]
fn concrete_host_lookup_stamps_durable_identity_not_predecessor_route() {
    use crate::lifecycle::{HostSessionCloseError, HostSessionCloseRequest, HostTransport};
    struct Transport(std::sync::Mutex<Option<std::sync::mpsc::Sender<()>>>);
    impl HostTransport for Transport {
        fn daemon_reachable(&self, _: &str) -> bool {
            true
        }
        fn live_session_paths(&self, _: &str) -> Result<Vec<String>, String> {
            Ok(vec![])
        }
        fn close_host_session(
            &self,
            _: &HostSessionCloseRequest,
        ) -> Result<(), HostSessionCloseError> {
            Ok(())
        }
        fn durable_session_id(
            &self,
            _: &crate::state::HostSessionIdentity,
        ) -> Result<Option<String>, String> {
            Ok(Some("durable-id".into()))
        }
        fn publish_task_status(
            &self,
            _: &crate::state::HostSessionIdentity,
            _: TaskStatus,
        ) -> Result<(), String> {
            if let Some(sender) = self.0.lock().unwrap().take() {
                sender.send(()).unwrap();
            }
            Ok(())
        }
    }
    let (sent, received) = std::sync::mpsc::channel();
    let transport = Arc::new(Transport(std::sync::Mutex::new(Some(sent))));
    let harness = super::fakes::make_manager(super::fakes::HarnessOptions {
        customize: Some(Box::new(move |options| {
            options.host_transport = Some(transport)
        })),
        ..Default::default()
    });
    let task = started(harness.manager.start(&base_spec()));
    harness
        .store
        .mutate(&task.task_id, |fresh: &crate::state::TaskRecord| {
            crate::state::TaskRecord {
                child_session_id: Some("predecessor".into()),
                host_session: Some(crate::state::HostSessionIdentity {
                    socket: "/host".into(),
                    routing_id: "route-not-id".into(),
                    session_path: "/session".into(),
                    instance_id: "host".into(),
                    daemon_pid: None,
                    effect_seq: None,
                    adopted_entry_id: None,
                }),
                ..fresh.clone()
            }
        })
        .unwrap();
    let (identity_sent, identity_received) = std::sync::mpsc::channel();
    let identity_store = harness.store.clone();
    let identity_task = task.task_id.clone();
    harness.store.set_mutation_listener(Some(Arc::new(move || {
        if identity_store
            .load(&identity_task)
            .unwrap()
            .is_some_and(|record| record.child_session_id.as_deref() == Some("durable-id"))
        {
            let _ = identity_sent.send(());
        }
    })));
    let fake = harness.in_process.wait_handle(&task.task_id);
    fake.emit(&ManagedChildEvent::of("session_info_changed"));
    identity_received.recv_timeout(super::fakes::WAIT).unwrap();
    harness.store.set_mutation_listener(None);
    assert_eq!(
        harness
            .store
            .load(&task.task_id)
            .unwrap()
            .unwrap()
            .child_session_id
            .as_deref(),
        Some("durable-id")
    );
    fake.complete("done");
    received.recv_timeout(super::fakes::WAIT).unwrap();
}
