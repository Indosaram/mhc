//! Tests for paused runs, backward-compatible persistence, paused workflow controls,
//! and paused-child recovery.

use std::fs;
use std::sync::{Arc, Mutex};

use pretty_assertions::assert_eq;

use crate::dag::graph::{DagCompileOptions, DagDefinition, DagNodeInput, compile_dag};
use crate::dag::manager::{
    DagAmendParams, DagManagerOptions, DagPersistedDefinition, DagPersistedNode,
    DagPersistedNodeTarget, DagRunRecordV1, create_dag_manager,
};
use crate::dag::node_retry::{reenter_dag_run, retry_dag_nodes};
use crate::dag::node_send::{DagNodeDeliveryResult, send_to_dag_node};
use crate::dag::paused_child_recovery::{
    PausedChildRecoveryPort, PausedChildStatus, RunPauseOwner, recover_paused_dag_children,
};
use crate::dag::paused_controls::{cancel_paused_dag_node, paused_amendment_unsafe};
use crate::dag::recovery::{DagRecoveryOptions, RecoverableRecord, create_dag_recovery};
use crate::dag::scheduler::{DagNodeControlErrorCode, DagSchedulerOptions};
use crate::dag::store::{DagStoreConfig, DagStoreOptions, create_dag_file_store};
use crate::dag::types::{
    DagEventLane, DagNode, DagNodeErrorCode, DagNodeState, DagNodeTarget,
    DagNodeTransitionReason, DagRunEvent, DagRunEventPayload, DagRunId, DagRunPause,
    DagRunStatus, SchemaVersion1,
};
use crate::manager::manager_tests::fakes::{Harness, HarnessOptions, base_spec, make_manager, started};

const AT: &str = "2026-08-14T00:00:00.000Z";

struct PausedFixture {
    harness: Harness,
    store: Arc<crate::dag::store::DagFileStore>,
    run_id: DagRunId,
    task_id: String,
    definition: DagDefinition,
    record: DagRunRecordV1,
    options: DagSchedulerOptions,
}

fn node_input(id: &str, depends_on: &[&str]) -> DagNodeInput {
    DagNodeInput {
        id: id.to_string(),
        prompt: format!("do {id}"),
        target: DagNodeTarget::Category("quick".to_string()),
        label: None,
        depends_on: if depends_on.is_empty() {
            None
        } else {
            Some(depends_on.iter().map(|d| d.to_string()).collect())
        },
        task_summary: None,
        description: None,
        load_skills: None,
    }
}

fn setup_paused_run() -> PausedFixture {
    let harness = make_manager(HarnessOptions::default());
    let started_task = started(harness.manager.start(&base_spec()));
    let task_id = started_task.task_id;

    let definition = DagDefinition {
        key: "paused-controls".to_string(),
        name: "paused controls".to_string(),
        nodes: vec![
            node_input("a", &[]),
            node_input("b", &["a"]),
            node_input("c", &["b"]),
        ],
    };

    let compiled = compile_dag(
        &definition,
        &DagCompileOptions {
            at: Some(AT.to_string()),
            settings: None,
        },
    );
    assert!(compiled.ok);

    let run_id = "paused-controls".to_string();
    let store = Arc::new(
        create_dag_file_store(
            &DagStoreConfig::new(harness.project.dir.path()),
            DagStoreOptions::default(),
        )
        .expect("store"),
    );

    let nodes = compiled
        .nodes
        .into_iter()
        .map(|node| match node.id.as_str() {
            "a" => DagNode {
                state: DagNodeState::Completed,
                output: Some("keep-a".to_string()),
                ..node
            },
            "b" => DagNode {
                state: DagNodeState::Paused,
                task_id: Some(task_id.clone()),
                attempt: 1,
                ..node
            },
            _ => DagNode {
                state: DagNodeState::Blocked,
                ..node
            },
        })
        .collect();

    let persisted_nodes = definition
        .nodes
        .iter()
        .map(|n| DagPersistedNode {
            id: n.id.clone(),
            prompt: n.prompt.clone(),
            target: DagPersistedNodeTarget::Category {
                category: "quick".to_string(),
            },
            label: n.label.clone(),
            depends_on: n.depends_on.clone(),
            task_summary: n.task_summary.clone(),
            description: n.description.clone(),
            load_skills: n.load_skills.clone(),
            effective_prompt: n.prompt.clone(),
        })
        .collect();

    let record = DagRunRecordV1 {
        schema_version: SchemaVersion1,
        checkpoint_seq: 0,
        run_id: run_id.clone(),
        run_key: definition.key.clone(),
        name: definition.name.clone(),
        parent_session_id: "parent-1".to_string(),
        root_session_id: "parent-1".to_string(),
        definition_fingerprint: "fixture".to_string(),
        definition: DagPersistedDefinition {
            key: definition.key.clone(),
            name: definition.name.clone(),
            nodes: persisted_nodes,
        },
        status: DagRunStatus::Paused,
        pause: Some(DagRunPause {
            reason: "user_stop".to_string(),
            pause_seq: 7,
        }),
        generation: 1,
        created_at: AT.to_string(),
        updated_at: AT.to_string(),
        started_at: None,
        completed_at: None,
        nodes,
        edges: compiled.edges,
        waves: compiled.waves,
        critical_path: compiled.critical_path,
        bottlenecks: compiled.bottlenecks,
        diagnostics: compiled.diagnostics,
        amend_history: None,
        lease_holder_pid: None,
        previous_lease_holder_pid: None,
        guidance: None,
        retry_on_resume: None,
        cancel_requested_nodes: None,
        paused_child_acks: None,
    };

    store.write_checkpoint(&run_id, &record).expect("write checkpoint");

    let options = DagSchedulerOptions {
        store: Arc::clone(&store),
        task_manager: Arc::new(harness.manager.clone()),
        initial_record: record.clone(),
        execution_mode: None,
        ancestry: None,
        node_spawn_policy: None,
        subscriber_ring: None,
        pre_attached_tasks: None,
        schedule_retry: None,
        now: Some(Arc::new(|| 0)),
        resume_credential: None,
    };

    PausedFixture {
        harness,
        store,
        run_id,
        task_id,
        definition,
        record,
        options,
    }
}

#[test]
fn test_compat_paused_node_encoding_on_disk() {
    let fixture = setup_paused_run();
    let raw_text = fs::read_to_string(fixture.store.paths.run(&fixture.run_id)).expect("read run json");
    let raw: serde_json::Value = serde_json::from_str(&raw_text).expect("parse json");
    let nodes = raw["nodes"].as_array().expect("nodes array");
    for node in nodes {
        assert_ne!(node["state"], "paused", "disk format must not expose paused node state directly");
        if node["id"] == "b" {
            assert_eq!(node["state"], "running");
            assert_eq!(node["paused"], true);
        }
    }

    let read_back: DagRunRecordV1 = fixture
        .store
        .read_checkpoint(&fixture.run_id)
        .expect("read")
        .expect("exists");
    assert_eq!(
        read_back.nodes.iter().find(|n| n.id == "b").map(|n| n.state),
        Some(DagNodeState::Paused)
    );
}

#[test]
fn test_compat_paused_transition_event_on_disk() {
    let fixture = setup_paused_run();
    let event = DagRunEvent {
        schema_version: SchemaVersion1,
        run_id: fixture.run_id.clone(),
        seq: 1,
        at: AT.to_string(),
        lane: DagEventLane::Boundary,
        payload: DagRunEventPayload::NodeTransitioned {
            node_id: "c".to_string(),
            from: DagNodeState::Blocked,
            to: DagNodeState::Paused,
            reason: DagNodeTransitionReason::Resumed,
        },
    };
    fixture.store.append_event(&event).expect("append event");

    let event_text = fs::read_to_string(fixture.store.paths.event(&fixture.run_id)).expect("read event log");
    let line: serde_json::Value = serde_json::from_str(event_text.lines().last().unwrap()).expect("parse event json");
    assert_eq!(line["to"], "running");
    assert_eq!(line["toPaused"], true);

    let page = fixture
        .store
        .read_events(&fixture.run_id, 0, &crate::dag::store::DagEventReadOptions {
            limit: 10,
            through_seq: None,
            lane: None,
            types: None,
        })
        .expect("read events");
    assert_eq!(page.events.len(), 1);
    match &page.events[0].payload {
        DagRunEventPayload::NodeTransitioned { from, to, .. } => {
            assert_eq!(*from, DagNodeState::Blocked);
            assert_eq!(*to, DagNodeState::Paused);
        }
        _ => panic!("unexpected event payload"),
    }
}

#[test]
fn test_design_28_model_cannot_reenter_user_paused_workflow() {
    let fixture = setup_paused_run();
    let result = reenter_dag_run(fixture.options);
    assert!(result.is_err());
    let error = match result { Err(error) => error, Ok(_) => panic!("model reentered a user-paused workflow") };
    assert_eq!(error.code, DagNodeControlErrorCode::RunPausedByUser);

    let read_back: DagRunRecordV1 = fixture
        .store
        .read_checkpoint(&fixture.run_id)
        .expect("read")
        .expect("exists");
    assert_eq!(read_back.status, DagRunStatus::Paused);
}

#[test]
fn test_design_28_send_to_paused_node_stores_guidance_without_sending() {
    let fixture = setup_paused_run();
    let result = send_to_dag_node(&fixture.options, &fixture.run_id, &"b".into(), "new instructions")
        .expect("send to paused node");
    assert_eq!(result.delivery, DagNodeDeliveryResult::Guidance);
    assert!(result.guidance_id.is_some());

    let fresh: DagRunRecordV1 = fixture
        .store
        .read_checkpoint(&fixture.run_id)
        .expect("read")
        .expect("exists");
    let guidance = fresh.guidance.expect("guidance present");
    assert_eq!(guidance.len(), 1);
    assert_eq!(guidance[0].text, "new instructions");
    assert_eq!(
        guidance[0].scope,
        crate::dag::types::GuidanceScope::Node {
            node_id: "b".to_string()
        }
    );
    assert_eq!(guidance[0].state, crate::dag::types::GuidanceState::Draft);
}

#[test]
fn test_design_26_cancel_then_retry_records_staged_retry() {
    let fixture = setup_paused_run();
    let closed_holds = Arc::new(Mutex::new(Vec::new()));
    let closed_clone = Arc::clone(&closed_holds);

    cancel_paused_dag_node(
        &fixture.options,
        &fixture.run_id,
        &"b".into(),
        move |task_id: &str, _| {
            closed_clone.lock().unwrap().push(task_id.to_string());
        },
    )
    .expect("cancel node b");

    assert_eq!(*closed_holds.lock().unwrap(), vec![fixture.task_id.clone()]);

    let fresh: DagRunRecordV1 = fixture
        .store
        .read_checkpoint(&fixture.run_id)
        .expect("read")
        .expect("exists");
    assert_eq!(
        fresh.nodes.iter().find(|n| n.id == "b").map(|n| n.state),
        Some(DagNodeState::Cancelled)
    );

    let result = retry_dag_nodes(
        fixture.options.clone(),
        fixture.run_id.clone(),
        Some(&["b".into()]),
        None,
    )
    .expect("stage retry for node b");

    assert_eq!(result.record.status, DagRunStatus::Paused);
    assert_eq!(
        result.record.retry_on_resume,
        Some(vec!["b".to_string()])
    );

    let after_retry: DagRunRecordV1 = fixture
        .store
        .read_checkpoint(&fixture.run_id)
        .expect("read")
        .expect("exists");
    assert_eq!(
        after_retry.nodes.iter().find(|n| n.id == "c").map(|n| n.state),
        Some(DagNodeState::Blocked)
    );
}

#[test]
fn test_design_26_paused_amendment_safety() {
    let fixture = setup_paused_run();
    assert!(paused_amendment_unsafe(&fixture.record, "b"));

    let mut record_c_scheduled = fixture.record.clone();
    record_c_scheduled.nodes[2].state = DagNodeState::Scheduled;
    assert!(!paused_amendment_unsafe(&record_c_scheduled, "c"));
}

#[test]
fn test_design_26_staged_retry_runs_b_then_c_only_after_user_resumes() {
    let fixture = setup_paused_run();
    cancel_paused_dag_node(&fixture.options, &fixture.run_id, &"b".into(), |_, _| {})
        .expect("cancel node b");

    let staged = retry_dag_nodes(
        fixture.options.clone(),
        fixture.run_id.clone(),
        Some(&["b".into()]),
        None,
    )
    .expect("stage retry for node b");
    staged.scheduler.suspend();

    *fixture.harness.in_process.hook.lock().unwrap() = Some(Arc::new(|spec, _| {
        let handle = crate::manager::manager_tests::fakes::FakeHandle::new(&spec.task_id, None);
        handle.complete("done");
        Some(Ok(handle as Arc<dyn crate::manager::ManagedChildHandle>))
    }));

    let recovery = create_dag_recovery(DagRecoveryOptions {
        store: Arc::clone(&fixture.store),
        task_manager: Arc::clone(&fixture.options.task_manager),
        host_pid: Some(101),
        is_process_alive: Some(Arc::new(|_| false)),
        is_run_held_in_process: None,
        now: None,
        subscriber_ring: None,
        stop_admission: None,
        reattach: None,
        ancestry: None,
        node_spawn_policy: None,
        paused_children: None,
        resume_credential: None,
    });

    let installed = recovery
        .install_resumed_run(&fixture.run_id, "parent-1", 7)
        .expect("install resumed run")
        .unwrap_installed();
    let outcome = installed.wait();
    assert!(matches!(outcome, crate::dag::recovery::DagRecoveryOutcome::Resumed { .. }));

    let fresh: DagRunRecordV1 = fixture
        .store
        .read_checkpoint(&fixture.run_id)
        .expect("read")
        .expect("exists");
    let states: Vec<_> = fresh.nodes.iter().map(|n| n.state).collect();
    assert_eq!(
        states,
        vec![
            DagNodeState::Completed,
            DagNodeState::Completed,
            DagNodeState::Completed
        ]
    );
    assert_eq!(
        fresh
            .nodes
            .iter()
            .find(|n| n.id == "a")
            .and_then(|n| n.output.as_deref()),
        Some("keep-a")
    );
}

#[test]
fn test_design_26_cancelled_b_makes_c_skipped_when_user_resumes() {
    let fixture = setup_paused_run();
    cancel_paused_dag_node(&fixture.options, &fixture.run_id, &"b".into(), |_, _| {})
        .expect("cancel node b");

    let recovery = create_dag_recovery(DagRecoveryOptions {
        store: Arc::clone(&fixture.store),
        task_manager: Arc::clone(&fixture.options.task_manager),
        host_pid: Some(101),
        is_process_alive: Some(Arc::new(|_| false)),
        is_run_held_in_process: None,
        now: None,
        subscriber_ring: None,
        stop_admission: None,
        reattach: None,
        ancestry: None,
        node_spawn_policy: None,
        paused_children: None,
        resume_credential: None,
    });

    let installed = recovery
        .install_resumed_run(&fixture.run_id, "parent-1", 7)
        .expect("install resumed run")
        .unwrap_installed();
    let outcome = installed.wait();
    assert!(matches!(outcome, crate::dag::recovery::DagRecoveryOutcome::Resumed { .. }));

    let fresh: DagRunRecordV1 = fixture
        .store
        .read_checkpoint(&fixture.run_id)
        .expect("read")
        .expect("exists");
    assert_eq!(
        fresh.nodes.iter().find(|n| n.id == "c").map(|n| n.state),
        Some(DagNodeState::Skipped)
    );
}

#[test]
fn test_design_26_amending_queued_c_while_paused_never_reenters_or_changes_paused_b() {
    let fixture = setup_paused_run();
    let manager = create_dag_manager(DagManagerOptions {
        store: Arc::clone(&fixture.store),
        now: Some(Arc::new(|| 0)),
        new_run_id: None,
        materialize_skills: None,
        settings: None,
    });

    let mut changed_def = fixture.definition.clone();
    changed_def.nodes[2].prompt = "updated-c".to_string();

    let amended = manager
        .amend(DagAmendParams {
            run_id: Some(fixture.run_id.clone()),
            key: None,
            definition: changed_def,
            parent_session_id: "parent-1".to_string(),
        })
        .expect("amend c succeeds");
    assert_eq!(
        amended.pause,
        Some(DagRunPause {
            reason: "user_stop".to_string(),
            pause_seq: 7,
        })
    );
    assert_eq!(
        amended.nodes.iter().find(|n| n.id == "b").map(|n| n.state),
        Some(DagNodeState::Paused)
    );

    let mut unsafe_def = fixture.definition.clone();
    unsafe_def.nodes[1].prompt = "unsafe-b".to_string();
    let err = manager
        .amend(DagAmendParams {
            run_id: Some(fixture.run_id.clone()),
            key: None,
            definition: unsafe_def,
            parent_session_id: "parent-1".to_string(),
        })
        .expect_err("amend b must fail");
    assert_eq!(err.code, crate::dag::manager::DagManagerErrorCode::AmendRunningNode);
}

struct MockPausedPort {
    inspect_result: PausedChildStatus,
    restore_calls: Arc<Mutex<Vec<(String, RunPauseOwner)>>>,
    restore_error: Option<String>,
    restore_seq: u64,
}

impl PausedChildRecoveryPort for MockPausedPort {
    fn inspect(&self, _task_id: &str) -> PausedChildStatus {
        self.inspect_result
    }

    fn restore_paused(&self, task_id: &str, owner: &RunPauseOwner) -> Result<u64, String> {
        self.restore_calls
            .lock()
            .unwrap()
            .push((task_id.to_string(), owner.clone()));
        if let Some(err) = &self.restore_error {
            Err(err.clone())
        } else {
            Ok(self.restore_seq)
        }
    }
}

#[test]
fn test_design_29_lost_paused_child_fails_explicitly_and_is_never_redispatched() {
    let fixture = setup_paused_run();
    let port = MockPausedPort {
        inspect_result: PausedChildStatus::Lost,
        restore_calls: Arc::new(Mutex::new(Vec::new())),
        restore_error: None,
        restore_seq: 1,
    };

    recover_paused_dag_children(&fixture.options, &port).expect("recover paused children");

    let fresh: DagRunRecordV1 = fixture
        .store
        .read_checkpoint(&fixture.run_id)
        .expect("read")
        .expect("exists");
    let node_b = fresh.nodes.iter().find(|n| n.id == "b").expect("node b");
    assert_eq!(node_b.state, DagNodeState::Failed);
    assert_eq!(
        node_b.error.as_ref().map(|e| e.code),
        Some(DagNodeErrorCode::LostWhilePaused)
    );
    assert_eq!(
        fresh.nodes.iter().find(|n| n.id == "c").map(|n| n.state),
        Some(DagNodeState::Blocked)
    );
}

#[test]
fn test_design_29_failed_paused_restore_releases_claimed_run_lease() {
    let fixture = setup_paused_run();
    let port = Arc::new(MockPausedPort {
        inspect_result: PausedChildStatus::Restorable,
        restore_calls: Arc::new(Mutex::new(Vec::new())),
        restore_error: Some("restore failed".to_string()),
        restore_seq: 1,
    });

    let recovery = create_dag_recovery(DagRecoveryOptions {
        store: Arc::clone(&fixture.store),
        task_manager: Arc::clone(&fixture.options.task_manager),
        host_pid: Some(101),
        is_process_alive: Some(Arc::new(|_| false)),
        is_run_held_in_process: None,
        now: None,
        subscriber_ring: None,
        stop_admission: None,
        reattach: None,
        ancestry: None,
        node_spawn_policy: None,
        paused_children: Some(port),
        resume_credential: None,
    });

    let result = recovery.install_resumed_run(&fixture.run_id, "parent-1", 7);
    assert!(result.is_err());

    let fresh: RecoverableRecord = fixture
        .store
        .read_checkpoint(&fixture.run_id)
        .expect("read")
        .expect("exists");
    assert_eq!(fresh.lease_holder_pid, None);
    assert_eq!(fresh.record.status, DagRunStatus::Paused);
}

#[test]
fn test_design_29_restorable_child_is_restored_under_inherited_custody() {
    let fixture = setup_paused_run();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let port = MockPausedPort {
        inspect_result: PausedChildStatus::Restorable,
        restore_calls: Arc::clone(&calls),
        restore_error: None,
        restore_seq: 11,
    };

    recover_paused_dag_children(&fixture.options, &port).expect("recover paused children");

    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, fixture.task_id);
    assert_eq!(calls[0].1.parent_session_id, "parent-1");
    assert_eq!(calls[0].1.parent_pause_seq, 7);

    let fresh: DagRunRecordV1 = fixture
        .store
        .read_checkpoint(&fixture.run_id)
        .expect("read")
        .expect("exists");
    let node_b = fresh.nodes.iter().find(|n| n.id == "b").expect("node b");
    assert_eq!(node_b.state, DagNodeState::Paused);
    assert_eq!(node_b.task_id, Some(fixture.task_id.clone()));
    assert_eq!(
        fresh.paused_child_acks.as_ref().and_then(|m| m.get("b")),
        Some(&11)
    );
}
