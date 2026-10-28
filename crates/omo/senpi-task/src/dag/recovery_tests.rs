//! `dag/recovery.test.ts`. Covers the load-bearing crash-recovery invariants (paused-run resume
//! with output reuse, foreign-session exclusion, live-lease skip via the default probe, and
//! shutdown-pause lease release) against the real `TaskManager` + `FakeRunner` fixture from
//! `manager::manager_tests::fakes`. Rust's `TaskManager` is a concrete struct rather than the
//! pluggable interface the TS file mocks directly with a hand-rolled `RecoveryTaskManager`, so
//! this file's coverage is narrower than the pinned 10-case suite: the WAL-crash-injection cases
//! (which the TS file drives by overriding individual `DagFileStore` methods, a capability the
//! Rust `DagFileStore` does not expose since it is a concrete struct, not an object literal) and
//! the two-manager lease race are tracked as an N/A gap in the parity row.

use std::sync::atomic::{AtomicI64, Ordering};

use pretty_assertions::assert_eq;

use super::*;
use crate::dag::graph::{DagDefinition, DagNodeInput};
use crate::dag::manager::DagRunRecordV1;
use crate::dag::store::{DagStoreConfig, DagStoreOptions, create_dag_file_store};
use crate::dag::types::{DagNodeState, DagNodeTarget, DagRunStatus, SchemaVersion1};
use crate::manager::manager_tests::fakes::{HarnessOptions, config, make_manager};

const PARENT_SESSION_ID: &str = "session-parent";
const ROOT_SESSION_ID: &str = "session-root";
const AT: &str = "2026-08-14T00:00:00.000Z";

fn run_id() -> DagRunId {
    "run-recovery".to_string()
}

fn node(id: &str, depends_on: &[&str]) -> DagNodeInput {
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

fn definition(nodes: Vec<DagNodeInput>) -> DagDefinition {
    DagDefinition {
        key: "recovery-test".to_string(),
        name: "recovery test".to_string(),
        nodes,
    }
}

fn temp_store() -> Arc<DagFileStore> {
    let path = tempfile::tempdir().expect("tempdir").keep();
    Arc::new(
        create_dag_file_store(&DagStoreConfig::new(path), DagStoreOptions::default())
            .expect("store opens"),
    )
}

fn base_record(input: &DagDefinition, node_states: &[(&str, DagNodeState)]) -> RecoverableRecord {
    let compiled = crate::dag::graph::compile_dag(
        input,
        &crate::dag::graph::DagCompileOptions {
            at: Some(AT.to_string()),
            settings: None,
        },
    );
    assert!(compiled.ok, "test DAG did not compile: {:?}", compiled.errors);
    let nodes: Vec<DagNode> = compiled
        .nodes
        .into_iter()
        .map(|mut node| {
            if let Some((_, state)) = node_states.iter().find(|(id, _)| *id == node.id) {
                node.state = *state;
            }
            node
        })
        .collect();
    let record = DagRunRecordV1 {
        schema_version: SchemaVersion1,
        checkpoint_seq: 0,
        run_id: run_id(),
        run_key: input.key.clone(),
        name: input.name.clone(),
        parent_session_id: PARENT_SESSION_ID.to_string(),
        root_session_id: ROOT_SESSION_ID.to_string(),
        definition_fingerprint: "definition-fingerprint".to_string(),
        definition: crate::dag::manager::DagPersistedDefinition {
            key: input.key.clone(),
            name: input.name.clone(),
            nodes: input
                .nodes
                .iter()
                .map(|entry| crate::dag::manager::DagPersistedNode {
                    id: entry.id.clone(),
                    prompt: entry.prompt.clone(),
                    target: crate::dag::manager::DagPersistedNodeTarget::Category {
                        category: "quick".to_string(),
                    },
                    label: entry.label.clone(),
                    depends_on: entry.depends_on.clone(),
                    task_summary: entry.task_summary.clone(),
                    description: entry.description.clone(),
                    load_skills: entry.load_skills.clone(),
                    effective_prompt: entry.prompt.clone(),
                })
                .collect(),
        },
        status: DagRunStatus::Paused,
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
        pause: None,
    };
    RecoverableRecord {
        record,
        lease_holder_pid: None,
        previous_lease_holder_pid: None,
    }
}

#[test]
fn given_a_paused_run_after_wave_one_when_the_session_restarts_then_the_incomplete_wave_resumes_and_completes()
 {
    let store = temp_store();
    let input = definition(vec![node("done", &[]), node("next", &["done"])]);
    let mut record = base_record(&input, &[("done", DagNodeState::Completed), ("next", DagNodeState::Blocked)]);
    record.previous_lease_holder_pid = Some(9001);
    store
        .write_checkpoint(&run_id(), &record)
        .expect("write checkpoint");
    store
        .write_result(&run_id(), "done", "durable done output")
        .expect("write result");
    let harness = make_manager(HarnessOptions {
        config: Some(config(5, 1)),
        ..HarnessOptions::default()
    });
    let recovery = create_dag_recovery(DagRecoveryOptions {
        store: Arc::clone(&store),
        task_manager: Arc::new(harness.manager),
        host_pid: Some(101),
        is_process_alive: Some(Arc::new(|pid: i64| pid == 101)),
        is_run_held_in_process: None,
        now: None,
        subscriber_ring: None,
        stop_admission: None,
        reattach: None,
        ancestry: None,
        node_spawn_policy: None, paused_children: None, resume_credential: None,
    });

    let outcomes = std::thread::spawn({
        let harness_runner = Arc::clone(&harness.in_process);
        let store_for_completions = Arc::clone(&store);
        move || {
            std::thread::spawn(move || {
                let handle = crate::dag::test_support::wait_for_attached_handle(
                    &store_for_completions,
                    &harness_runner,
                    &run_id(),
                    "next",
                );
                handle.complete("done next");
            });
            recovery.resume_paused_runs(PARENT_SESSION_ID)
        }
    })
    .join()
    .expect("recovery thread");

    assert_eq!(outcomes.len(), 1);
    match &outcomes[0] {
        DagRecoveryOutcome::Resumed { record, reused_outputs, .. } => {
            assert_eq!(reused_outputs.get("done"), Some(&"durable done output".to_string()));
            assert_eq!(
                record
                    .nodes
                    .iter()
                    .map(|n| format!("{}:{}", n.id, state_str(n.state)))
                    .collect::<Vec<_>>(),
                vec!["done:completed".to_string(), "next:completed".to_string()]
            );
        }
        other => panic!("expected resumed, got {other:?}"),
    }
}

fn state_str(state: DagNodeState) -> &'static str {
    match state {
        DagNodeState::Pending => "pending",
        DagNodeState::Blocked => "blocked",
        DagNodeState::Scheduled => "scheduled",
        DagNodeState::Paused => "paused",
        DagNodeState::Running => "running",
        DagNodeState::Completed => "completed",
        DagNodeState::Failed => "failed",
        DagNodeState::Cancelled => "cancelled",
        DagNodeState::Skipped => "skipped",
    }
}

#[test]
fn given_no_injected_liveness_probe_when_a_paused_runs_previous_holder_is_this_live_process_then_the_default_probe_skips_it_as_a_live_lease()
 {
    let store = temp_store();
    let input = definition(vec![node("only", &[])]);
    let mut record = base_record(&input, &[("only", DagNodeState::Scheduled)]);
    record.previous_lease_holder_pid = Some(i64::from(std::process::id()));
    store.write_checkpoint(&run_id(), &record).expect("write checkpoint");
    let runner = crate::manager::manager_tests::fakes::FakeRunner::new();
    let harness = make_manager(HarnessOptions { in_process: Some(Arc::clone(&runner)), ..HarnessOptions::default() });
    let recovery = create_dag_recovery(DagRecoveryOptions {
        store: Arc::clone(&store),
        task_manager: Arc::new(harness.manager),
        host_pid: Some(101),
        is_process_alive: None,
        is_run_held_in_process: None,
        now: None,
        subscriber_ring: None,
        stop_admission: None,
        reattach: None,
        ancestry: None,
        node_spawn_policy: None, paused_children: None, resume_credential: None,
    });

    let outcomes = recovery.resume_paused_runs(PARENT_SESSION_ID);

    assert_eq!(outcomes.len(), 1);
    assert_eq!(
        outcomes[0],
        DagRecoveryOutcome::Skipped {
            run_id: run_id(),
            reason: DagRecoverySkipReason::LiveLease,
            holder_pid: Some(i64::from(std::process::id())),
        }
    );
}

#[test]
fn given_a_paused_own_host_run_without_a_registered_scheduler_when_it_resumes_then_it_claims_instead_of_waiting_on_itself() {
    let store = temp_store();
    let input = definition(vec![node("only", &[])]);
    let mut record = base_record(&input, &[("only", DagNodeState::Scheduled)]);
    record.previous_lease_holder_pid = Some(101);
    store.write_checkpoint(&run_id(), &record).expect("checkpoint");
    let harness = make_manager(HarnessOptions::default());
    *harness.in_process.hook.lock().unwrap() = Some(Arc::new(|spec, _| {
        let handle = crate::manager::manager_tests::fakes::FakeHandle::new(&spec.task_id, None);
        handle.complete("claimed run completed");
        Some(Ok(handle as Arc<dyn crate::manager::ManagedChildHandle>))
    }));
    let recovery = create_dag_recovery(DagRecoveryOptions {
        store: Arc::clone(&store), task_manager: Arc::new(harness.manager), host_pid: Some(101),
        is_process_alive: Some(Arc::new(|_| true)), is_run_held_in_process: None, now: None,
        subscriber_ring: None, stop_admission: None, reattach: None, ancestry: None, node_spawn_policy: None, paused_children: None, resume_credential: None,
    });
    let outcomes = recovery.resume_paused_runs(PARENT_SESSION_ID);
    assert!(matches!(outcomes.first(), Some(DagRecoveryOutcome::Resumed { .. })));
}

#[test]
fn given_no_injected_liveness_probe_when_a_paused_runs_previous_holder_pid_does_not_exist_then_the_default_probe_claims_the_run()
 {
    let store = temp_store();
    let input = definition(vec![node("only", &[])]);
    let mut record = base_record(&input, &[("only", DagNodeState::Scheduled)]);
    record.previous_lease_holder_pid = Some(2_147_483_647);
    store.write_checkpoint(&run_id(), &record).expect("write checkpoint");
    let harness = make_manager(HarnessOptions::default());
    let runner = Arc::clone(&harness.in_process);
    let store_for_completions = Arc::clone(&store);
    let recovery = create_dag_recovery(DagRecoveryOptions {
        store: Arc::clone(&store),
        task_manager: Arc::new(harness.manager),
        host_pid: Some(101),
        is_process_alive: None,
        is_run_held_in_process: None,
        now: None,
        subscriber_ring: None,
        stop_admission: None,
        reattach: None,
        ancestry: None,
        node_spawn_policy: None, paused_children: None, resume_credential: None,
    });

    let done = std::thread::spawn(move || {
        let handle = crate::dag::test_support::wait_for_attached_handle(
            &store_for_completions,
            &runner,
            &run_id(),
            "only",
        );
        handle.complete("done only");
    });
    let outcomes = recovery.resume_paused_runs(PARENT_SESSION_ID);
    done.join().expect("completion thread");

    assert_eq!(outcomes.len(), 1);
    assert!(
        matches!(outcomes[0], DagRecoveryOutcome::Resumed { .. }),
        "expected resumed, got {:?}",
        outcomes[0]
    );
}

#[test]
fn given_a_paused_run_owned_by_another_parent_session_when_recovery_scans_then_it_is_not_claimed() {
    let store = temp_store();
    let input = definition(vec![node("foreign", &[])]);
    let mut record = base_record(&input, &[]);
    record.record.parent_session_id = "foreign-session".to_string();
    record.previous_lease_holder_pid = Some(9001);
    store.write_checkpoint(&run_id(), &record).expect("write checkpoint");
    let harness = make_manager(HarnessOptions::default());
    let recovery = create_dag_recovery(DagRecoveryOptions {
        store: Arc::clone(&store),
        task_manager: Arc::new(harness.manager),
        host_pid: Some(101),
        is_process_alive: Some(Arc::new(|_| false)),
        is_run_held_in_process: None,
        now: None,
        subscriber_ring: None,
        stop_admission: None,
        reattach: None,
        ancestry: None,
        node_spawn_policy: None, paused_children: None, resume_credential: None,
    });

    let outcomes = recovery.resume_paused_runs(PARENT_SESSION_ID);

    assert_eq!(outcomes, Vec::new());
    assert_eq!(
        store
            .read_checkpoint::<RecoverableRecord>(&run_id())
            .expect("read checkpoint")
            .map(|r| r.record.status),
        Some(DagRunStatus::Paused)
    );
}

#[test]
fn given_a_dead_immediate_fork_source_when_the_child_session_resumes_then_the_run_is_adopted() {
    let store = temp_store();
    let input = definition(vec![node("work", &[])]);
    let mut record = base_record(&input, &[]);
    record.record.parent_session_id = "source-session".to_string();
    record.record.root_session_id = "source-session".to_string();
    record.previous_lease_holder_pid = Some(9001);
    store.write_checkpoint(&run_id(), &record).expect("write checkpoint");
    let harness = make_manager(HarnessOptions::default());
    let recovery = create_dag_recovery(DagRecoveryOptions {
        store: Arc::clone(&store), task_manager: Arc::new(harness.manager), host_pid: Some(101),
        is_process_alive: Some(Arc::new(|_| false)), is_run_held_in_process: None, now: None,
        subscriber_ring: None, stop_admission: None, reattach: None, ancestry: Some(DagSchedulerAncestry { depth: 2 }), node_spawn_policy: None, paused_children: None, resume_credential: None,
    });
    let outcomes = recovery.resume_paused_runs_from("child-session", Some(" source-session "));
    assert!(matches!(outcomes.first(), Some(DagRecoveryOutcome::Adopted { .. })));
    let adopted = store.read_checkpoint::<RecoverableRecord>(&run_id()).expect("read").expect("record");
    assert_eq!(adopted.record.parent_session_id, "child-session");
    assert_eq!(adopted.record.root_session_id, "child-session");
}

#[test]
fn given_a_live_resident_child_when_recovery_resumes_then_it_is_handed_to_the_scheduler_without_blocking() {
    use std::sync::mpsc;
    let store = temp_store();
    let input = definition(vec![node("slow", &[]), node("done", &[]), node("next", &["done"])]);
    let mut record = base_record(&input, &[("slow", DagNodeState::Running), ("done", DagNodeState::Completed), ("next", DagNodeState::Pending)]);
    record.record.nodes[0].task_id = Some("st_00000007".into());
    record.record.nodes[1].task_id = Some("st_00000008".into());
    record.record.nodes[0].attempt = 1;
    record.previous_lease_holder_pid = Some(9001);
    store.write_checkpoint(&run_id(), &record).expect("checkpoint");
    store.write_result(&run_id(), "done", "durable done output").expect("result");
    let runner = crate::manager::manager_tests::fakes::FakeRunner::new();
    let harness = make_manager(HarnessOptions { in_process: Some(Arc::clone(&runner)), ..HarnessOptions::default() });
    let manager = Arc::new(harness.manager);
    let started = manager.start_owned(&crate::manager::types::ManagerStartSpec {
        prompt: "do slow".into(), parent_session_id: PARENT_SESSION_ID.into(),
        root_session_id: Some(ROOT_SESSION_ID.into()), depth: 1, category: Some("quick".into()),
        name: Some("slow".into()), run_in_background: true, ..Default::default()
    }, &DagTaskOwner { kind: DagOwnerKind::Dag, run_id: run_id(), node_id: "slow".into(), fingerprint: "unused".into() });
    let slow_id = match started { OwnedStartResult::Started { task, .. } => task.task_id, other => panic!("task start failed: {other:?}") };
    let parsed_slow_id = crate::state::parse_task_id(&slow_id).expect("canonical task id");
    let record_task_id = parsed_slow_id.to_string();
    let mut task_record = manager.get(&slow_id).expect("resident task record");
    task_record.task_id = record_task_id.clone();
    harness.store.replace(&task_record).expect("persist canonical resident id");
    let handle = runner.wait_handle(&slow_id);
    let expected_task_id = record_task_id.clone();
    record.record.nodes[0].task_id = Some(record_task_id.clone());
    store.write_checkpoint(&run_id(), &record).expect("checkpoint with resident task id");
    let (reattach_tx, reattach_rx) = mpsc::channel();
    let recovery = create_dag_recovery(DagRecoveryOptions {
        store: Arc::clone(&store), task_manager: Arc::clone(&manager), host_pid: Some(101),
        is_process_alive: Some(Arc::new(|_| false)), is_run_held_in_process: None, now: None,
        subscriber_ring: None, stop_admission: None,
        reattach: Some(Arc::new(move |_, task_id| { let _ = reattach_tx.send(task_id.to_string()); })),
        ancestry: None, node_spawn_policy: None, paused_children: None, resume_credential: None,
    });
    let (resumed_tx, resumed_rx) = mpsc::channel();
    let run_id_for_listener = run_id();
    let _unsubscribe = crate::dag::journal::subscribe_dag_journal(
        &store,
        &run_id_for_listener,
        Arc::new(move |event| {
            if matches!(&event.payload, DagRunEventPayload::RunResumed { .. }) {
                let _ = resumed_tx.send(());
            }
        }),
    );
    let done = std::thread::spawn(move || recovery.resume_paused_runs(PARENT_SESSION_ID));
    assert_eq!(reattach_rx.recv_timeout(std::time::Duration::from_secs(30)).expect("reattach callback"), expected_task_id);
    resumed_rx.recv_timeout(std::time::Duration::from_secs(30)).expect("run resumed event");
    let checkpoint = store.read_checkpoint::<DagRunRecordV1>(&run_id()).expect("read").expect("record");
    assert_eq!(checkpoint.status, DagRunStatus::Running);
    assert_eq!(checkpoint.nodes[0].state, DagNodeState::Running);
    assert!(store.read_events(&run_id(), 0, &crate::dag::store::DagEventReadOptions { limit: 100, ..Default::default() }).expect("events").events.iter().any(|event| matches!(&event.payload, DagRunEventPayload::NodeReused { node_id, .. } if node_id == "done")));
    handle.complete("slow done");
    crate::dag::test_support::wait_for_attached_handle(&store, &runner, &run_id(), "next").complete("next done");
    let outcomes = done.join().expect("resume thread");
    let result = recovered(outcomes);
    assert_eq!(result.status, DagRunStatus::Completed);
    assert_eq!(result.nodes.iter().map(|n| format!("{}:{}", n.id, state_str(n.state))).collect::<Vec<_>>(), vec!["slow:completed", "done:completed", "next:completed"]);
}

#[test]
fn given_running_tasks_without_a_local_resident_handle_when_recovery_resumes_then_nodes_fail_as_orphaned() {
    let store = temp_store();
    let input = definition(vec![node("parked", &[]), node("foreign", &[]), node("gate", &["parked", "foreign"])]);
    let mut record = base_record(&input, &[("parked", DagNodeState::Running), ("foreign", DagNodeState::Running)]);
    record.record.nodes[0].task_id = Some("st_00000001".into());
    record.record.nodes[1].task_id = Some("st_00000002".into());
    record.previous_lease_holder_pid = Some(9001);
    store.write_checkpoint(&run_id(), &record).expect("checkpoint");
    let harness = make_manager(HarnessOptions::default());
    for (id, status, task_id) in [("parked", TaskStatus::Running, "st_00000001"), ("foreign", TaskStatus::Running, "st_00000002")] {
        let mut task = durable_task(&harness, id, status);
        task.task_id = task_id.into();
        task.residency_state = if id == "parked" { crate::state::ResidencyState::RpcDetached } else { crate::state::ResidencyState::Resident };
        task.host_pid = Some(if id == "foreign" { 9002 } else { 101 });
        harness.store.replace(&task).expect("persist fixture details");
    }
    let recovery = create_dag_recovery(DagRecoveryOptions {
        store: Arc::clone(&store), task_manager: Arc::new(harness.manager), host_pid: Some(101),
        is_process_alive: Some(Arc::new(|_| false)), is_run_held_in_process: None, now: None,
        subscriber_ring: None, stop_admission: None, reattach: None, ancestry: None, node_spawn_policy: None, paused_children: None, resume_credential: None,
    });
    let result = recovered(recovery.resume_paused_runs(PARENT_SESSION_ID));
    assert_eq!(result.nodes[0].error.as_ref().map(|e| e.code), Some(DagNodeErrorCode::ResumeTaskOrphaned));
    assert!(result.nodes[0].error.as_ref().is_some_and(|e| e.message.contains("rpc_detached")));
    assert_eq!(result.nodes[1].error.as_ref().map(|e| e.code), Some(DagNodeErrorCode::ResumeTaskOrphaned));
    assert!(result.nodes[1].error.as_ref().is_some_and(|e| e.message.contains("host_pid=Some(9002)")));
    assert_eq!(result.nodes[2].state, DagNodeState::Skipped);
}

#[test]
fn given_unstarted_lost_tasks_below_the_exec_attempt_limit_when_resumed_then_recovery_retries_them() {
    let store = temp_store();
    let input = definition(vec![node("queued", &[])]);
    let mut record = base_record(&input, &[("queued", DagNodeState::Scheduled)]);
    record.record.nodes[0].task_id = Some("st_00000003".into());
    record.record.nodes[0].attempt = 9;
    record.record.nodes[0].exec_attempt = Some(1);
    record.previous_lease_holder_pid = Some(9001);
    store.write_checkpoint(&run_id(), &record).expect("checkpoint");
    let harness = make_manager(HarnessOptions::default());
    let mut task = durable_task(&harness, "queued", TaskStatus::Lost);
    task.task_id = "st_00000003".into();
    harness.store.replace(&task).expect("persist lost task");
    *harness.in_process.hook.lock().unwrap() = Some(Arc::new(|spec, _| {
        let handle = crate::manager::manager_tests::fakes::FakeHandle::new(&spec.task_id, None);
        handle.complete("readmitted");
        Some(Ok(handle as Arc<dyn crate::manager::ManagedChildHandle>))
    }));
    let result = recovered(recovery_for(&store, Arc::new(harness.manager)).resume_paused_runs(PARENT_SESSION_ID));
    assert_eq!(result.nodes[0].state, DagNodeState::Completed);
    assert_eq!(result.nodes[0].attempt, 10);
    assert_eq!(result.nodes[0].exec_attempt, Some(2));
    assert!(store.read_events(&run_id(), 0, &crate::dag::store::DagEventReadOptions { limit: 100, ..Default::default() }).expect("events").events.iter().any(|event| matches!(&event.payload, DagRunEventPayload::NodeRetried { node_id, prior_task_id: Some(prior), exec_attempt: 2, .. } if node_id == "queued" && prior == "st_00000003")));
}

#[test]
fn given_a_lost_scheduled_task_with_started_at_when_resumed_then_it_fails_without_readmission() {
    let store = temp_store();
    let input = definition(vec![node("launched", &[])]);
    let mut record = base_record(&input, &[("launched", DagNodeState::Scheduled)]);
    record.record.nodes[0].task_id = Some("st_00000004".into());
    record.record.nodes[0].exec_attempt = Some(1);
    record.previous_lease_holder_pid = Some(9001);
    store.write_checkpoint(&run_id(), &record).expect("checkpoint");
    let harness = make_manager(HarnessOptions::default());
    let mut task = durable_task(&harness, "launched", TaskStatus::Lost);
    task.task_id = "st_00000004".into();
    harness.store.replace(&task).expect("persist canonical task id");
    task.started_at = Some(AT.to_string());
    harness.store.replace(&task).expect("persist launched loss");

    let result = recovered(recovery_for(&store, Arc::new(harness.manager)).resume_paused_runs(PARENT_SESSION_ID));

    assert_eq!(result.nodes[0].state, DagNodeState::Failed);
    assert_eq!(result.nodes[0].error.as_ref().map(|error| error.code), Some(DagNodeErrorCode::TaskLost));
    assert_eq!(result.nodes[0].exec_attempt, Some(1));
    assert!(!store.read_events(&run_id(), 0, &crate::dag::store::DagEventReadOptions { limit: 100, ..Default::default() }).expect("events").events.iter().any(|event| matches!(event.payload, DagRunEventPayload::NodeRetried { .. })));
}

#[test]
fn given_a_lost_unstarted_node_at_the_readmission_limit_when_resumed_then_it_fails_without_retry() {
    let store = temp_store();
    let input = definition(vec![node("queued", &[])]);
    let mut record = base_record(&input, &[("queued", DagNodeState::Scheduled)]);
    record.record.nodes[0].task_id = Some("st_00000005".into());
    record.record.nodes[0].exec_attempt = Some(3);
    record.previous_lease_holder_pid = Some(9001);
    store.write_checkpoint(&run_id(), &record).expect("checkpoint");
    let harness = make_manager(HarnessOptions::default());
    let mut task = durable_task(&harness, "queued", TaskStatus::Lost);
    task.task_id = "st_00000005".into();
    harness.store.replace(&task).expect("persist lost task");
    let result = recovered(recovery_for(&store, Arc::new(harness.manager)).resume_paused_runs(PARENT_SESSION_ID));
    assert_eq!(result.nodes[0].state, DagNodeState::Failed);
    assert_eq!(result.nodes[0].error.as_ref().map(|e| e.code), Some(DagNodeErrorCode::TaskLost));
    assert!(!store.read_events(&run_id(), 0, &crate::dag::store::DagEventReadOptions { limit: 100, ..Default::default() }).expect("events").events.iter().any(|event| matches!(event.payload, DagRunEventPayload::NodeRetried { .. })));
}

#[test]
fn given_own_host_lease_holder_when_no_scheduler_is_registered_then_recovery_claims_it() {
    let store = temp_store();
    let input = definition(vec![node("only", &[])]);
    let mut record = base_record(&input, &[("only", DagNodeState::Scheduled)]);
    record.previous_lease_holder_pid = Some(101);
    store.write_checkpoint(&run_id(), &record).expect("checkpoint");
    let harness = make_manager(HarnessOptions::default());
    *harness.in_process.hook.lock().unwrap() = Some(Arc::new(|spec, _| {
        let handle = crate::manager::manager_tests::fakes::FakeHandle::new(&spec.task_id, None);
        handle.complete("recovered");
        Some(Ok(handle as Arc<dyn crate::manager::ManagedChildHandle>))
    }));
    let recovery = create_dag_recovery(DagRecoveryOptions {
        store: Arc::clone(&store), task_manager: Arc::new(harness.manager), host_pid: Some(101),
        is_process_alive: Some(Arc::new(|_| true)), is_run_held_in_process: Some(Arc::new(|_| false)),
        now: None, subscriber_ring: None, stop_admission: None, reattach: None, ancestry: None, node_spawn_policy: None, paused_children: None, resume_credential: None,
    });
    assert!(!recovery.resume_paused_runs(PARENT_SESSION_ID).is_empty());
}

#[test]
fn given_own_host_lease_holder_with_registered_scheduler_when_recovery_claims_then_it_reports_its_holder_pid() {
    let store = temp_store();
    let input = definition(vec![node("only", &[])]);
    let mut record = base_record(&input, &[("only", DagNodeState::Scheduled)]);
    record.previous_lease_holder_pid = Some(101);
    store.write_checkpoint(&run_id(), &record).expect("checkpoint");
    let harness = make_manager(HarnessOptions::default());
    let recovery = create_dag_recovery(DagRecoveryOptions {
        store: Arc::clone(&store), task_manager: Arc::new(harness.manager), host_pid: Some(101),
        is_process_alive: Some(Arc::new(|_| true)), is_run_held_in_process: Some(Arc::new(|_| true)),
        now: None, subscriber_ring: None, stop_admission: None, reattach: None, ancestry: None, node_spawn_policy: None, paused_children: None, resume_credential: None,
    });
    assert_eq!(recovery.resume_paused_runs(PARENT_SESSION_ID), vec![DagRecoveryOutcome::Skipped {
        run_id: run_id(), reason: DagRecoverySkipReason::LiveLease, holder_pid: Some(101),
    }]);
}

#[test]
fn given_an_ordinary_session_resume_when_a_foreign_run_exists_then_foreign_checkpoint_remains_untouched() {
    let store = temp_store();
    let input = definition(vec![node("foreign", &[])]);
    let mut foreign = base_record(&input, &[]);
    foreign.record.parent_session_id = "foreign-session".into();
    foreign.record.root_session_id = "foreign-session".into();
    foreign.previous_lease_holder_pid = Some(9001);
    store.write_checkpoint(&run_id(), &foreign).expect("foreign checkpoint");
    let before = store.read_checkpoint::<RecoverableRecord>(&run_id()).expect("read").expect("foreign");
    let harness = make_manager(HarnessOptions::default());
    let recovery = create_dag_recovery(DagRecoveryOptions {
        store: Arc::clone(&store), task_manager: Arc::new(harness.manager), host_pid: Some(101),
        is_process_alive: Some(Arc::new(|_| false)), is_run_held_in_process: None, now: None,
        subscriber_ring: None, stop_admission: None, reattach: None, ancestry: None, node_spawn_policy: None, paused_children: None, resume_credential: None,
    });
    assert!(recovery.resume_paused_runs(PARENT_SESSION_ID).is_empty());
    assert_eq!(store.read_checkpoint::<RecoverableRecord>(&run_id()).expect("read").expect("foreign"), before);
}

#[test]
fn given_a_live_run_when_shutdown_pause_starts_then_admission_stops_before_pause_persistence_and_its_lease_is_released()
 {
    let store = temp_store();
    let input = definition(vec![node("active", &[])]);
    let mut record = base_record(&input, &[("active", DagNodeState::Running)]);
    record.record.status = DagRunStatus::Running;
    record.lease_holder_pid = Some(101);
    store.write_checkpoint(&run_id(), &record).expect("write checkpoint");
    let harness = make_manager(HarnessOptions::default());
    let order: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let order_for_closure = Arc::clone(&order);
    let now_ms = Arc::new(AtomicI64::new(
        chrono::DateTime::parse_from_rfc3339("2026-08-14T00:00:04.000Z")
            .expect("parse")
            .timestamp_millis(),
    ));
    let now_for_closure = Arc::clone(&now_ms);
    let recovery = create_dag_recovery(DagRecoveryOptions {
        store: Arc::clone(&store),
        task_manager: Arc::new(harness.manager),
        host_pid: Some(101),
        is_process_alive: None,
        is_run_held_in_process: None,
        now: Some(Arc::new(move || now_for_closure.load(Ordering::SeqCst))),
        subscriber_ring: None,
        stop_admission: Some(Arc::new(move |_run_id: &DagRunId| {
            order_for_closure.lock().unwrap().push("stop".to_string());
        })),
        reattach: None,
        ancestry: None,
        node_spawn_policy: None, paused_children: None, resume_credential: None,
    });

    let paused = recovery.pause_runs_for_shutdown(PARENT_SESSION_ID);

    let checkpoint = store
        .read_checkpoint::<RecoverableRecord>(&run_id())
        .expect("read checkpoint")
        .expect("checkpoint present");
    assert_eq!(paused, vec![run_id()]);
    assert_eq!(*order.lock().unwrap(), vec!["stop".to_string()]);
    assert_eq!(checkpoint.record.status, DagRunStatus::Paused);
    assert_eq!(checkpoint.lease_holder_pid, None);
    assert_eq!(checkpoint.previous_lease_holder_pid, Some(101));
}

fn recovery_for(store: &Arc<DagFileStore>, manager: Arc<crate::manager::TaskManager>) -> DagRecovery {
    create_dag_recovery(DagRecoveryOptions {
        store: Arc::clone(store), task_manager: manager, host_pid: Some(101), is_process_alive: Some(Arc::new(|_| false)), is_run_held_in_process: None, now: None, subscriber_ring: None, stop_admission: None, reattach: None, ancestry: None, node_spawn_policy: None, paused_children: None, resume_credential: None,
    })
}

#[test]
fn failed_pause_checkpoint_is_not_reported_as_success() {
    let root = tempfile::tempdir().expect("root");
    let store = Arc::new(create_dag_file_store(&DagStoreConfig::new(root.path()), DagStoreOptions::default()).expect("store"));
    let mut record = base_record(&definition(vec![node("active", &[])]), &[]);
    record.record.status = DagRunStatus::Running;
    store.write_checkpoint(&run_id(), &record).expect("initial checkpoint");
    store.set_checkpoint_hook(Arc::new(|_| Err(crate::dag::store::DagStoreError::Message("pause persistence denied".into()))));
    let harness = make_manager(HarnessOptions::default());
    let error = recovery_for(&store, Arc::new(harness.manager)).try_pause_runs_for_shutdown(PARENT_SESSION_ID).expect_err("pause fails");
    assert_eq!(error.to_string(), "pause persistence denied");
    assert_eq!(store.read_checkpoint::<RecoverableRecord>(&run_id()).expect("read").expect("record").record.status, DagRunStatus::Running);
}

#[test]
fn plain_scheduler_checkpoint_preserves_and_explicit_release_clears_lease() {
    let root = tempfile::tempdir().expect("root");
    let store = create_dag_file_store(&DagStoreConfig::new(root.path()), DagStoreOptions::default()).expect("store");
    let mut record = base_record(&definition(vec![node("active", &[])]), &[]);
    record.lease_holder_pid = Some(101);
    store.write_checkpoint(&run_id(), &record).expect("leased checkpoint");
    // TS: the store no longer carries the lease forward; a scheduler checkpoint preserves it
    // because the record it journals was read back from the leased checkpoint.
    let scheduler_record = store.read_checkpoint::<DagRunRecordV1>(&run_id()).expect("read").expect("record");
    assert_eq!(scheduler_record.lease_holder_pid, Some(101));
    store.write_checkpoint(&run_id(), &scheduler_record).expect("plain scheduler checkpoint");
    assert_eq!(store.read_checkpoint::<RecoverableRecord>(&run_id()).expect("read").expect("record").lease_holder_pid, Some(101));
    record.lease_holder_pid = None;
    store.write_checkpoint(&run_id(), &record).expect("release checkpoint");
    assert_eq!(store.read_checkpoint::<RecoverableRecord>(&run_id()).expect("read").expect("record").lease_holder_pid, None);
}

fn recovered(outcomes: Vec<DagRecoveryOutcome>) -> Box<DagRunRecordV1> {
    assert_eq!(outcomes.len(), 1);
    match outcomes.into_iter().next().unwrap() {
        DagRecoveryOutcome::Resumed { record, .. } | DagRecoveryOutcome::Adopted { record, .. } => record,
        other => panic!("expected resumed: {other:?}"),
    }
}

#[test]
fn attached_node_without_task_owner_result_or_transcript_fails_closed() {
    let store = temp_store();
    let mut record = base_record(&definition(vec![node("uncertain", &[])]), &[("uncertain", DagNodeState::Running)]);
    record.record.nodes[0].task_id = Some("missing".into());
    record.record.nodes[0].attempt = 1;
    store.write_checkpoint(&run_id(), &record).unwrap();
    let harness = make_manager(HarnessOptions::default());
    let result = recovered(recovery_for(&store, Arc::new(harness.manager)).resume_paused_runs(PARENT_SESSION_ID));
    assert_eq!(result.nodes[0].state, DagNodeState::Failed);
    assert_eq!(result.nodes[0].error.as_ref().unwrap().code, DagNodeErrorCode::ResumeTaskMissing);
    assert_eq!(harness.in_process.started_count(), 0);
}

fn durable_task(harness: &crate::manager::manager_tests::fakes::Harness, node_id: &str, status: TaskStatus) -> TaskRecord {
    let mut task = crate::state::create_task_record(crate::state::TaskRecordInput {
        parent_session_id: PARENT_SESSION_ID.into(), root_session_id: ROOT_SESSION_ID.into(), name: Some(node_id.into()), depth: 1, category: Some("quick".into()), model: "fake-model".into(), execution_mode: "in-process".into(), owner: Some(DagTaskOwner { kind: DagOwnerKind::Dag, run_id: run_id(), node_id: node_id.into(), fingerprint: "unused".into() }), ..Default::default()
    }, None).unwrap();
    task.status = status;
    task.error_message = (status == TaskStatus::Lost).then(|| "previous-process in-process".into());
    task.final_response = (status == TaskStatus::Completed).then(|| format!("done {node_id}"));
    harness.store.save(&task).unwrap();
    task
}

#[test]
fn reconciled_lost_child_folds_task_lost_without_redispatch() {
    let store = temp_store();
    let harness = make_manager(HarnessOptions::default());
    let task = durable_task(&harness, "lost", TaskStatus::Lost);
    let mut record = base_record(&definition(vec![node("lost", &[])]), &[("lost", DagNodeState::Running)]);
    record.record.nodes[0].task_id = Some(task.task_id);
    record.record.nodes[0].attempt = 1;
    store.write_checkpoint(&run_id(), &record).unwrap();
    let result = recovered(recovery_for(&store, Arc::new(harness.manager)).resume_paused_runs(PARENT_SESSION_ID));
    assert_eq!(result.nodes[0].state, DagNodeState::Failed);
    assert_eq!(result.nodes[0].error.as_ref().unwrap().code, DagNodeErrorCode::TaskLost);
    assert_eq!(harness.in_process.started_count(), 0);
}

#[test]
fn scheduled_owned_task_is_reused_and_only_fresh_work_spawns() {
    use crate::manager::ManagedChildHandle;
    let store = temp_store();
    let harness = make_manager(HarnessOptions::default());
    let owned = durable_task(&harness, "owned", TaskStatus::Completed);
    let record = base_record(&definition(vec![node("owned", &[]), node("fresh", &[])]), &[("owned", DagNodeState::Scheduled), ("fresh", DagNodeState::Scheduled)]);
    store.write_checkpoint(&run_id(), &record).unwrap();
    *harness.in_process.hook.lock().unwrap() = Some(Arc::new(|spec, _| {
        let handle = crate::manager::manager_tests::fakes::FakeHandle::new(&spec.task_id, None);
        handle.complete("done fresh");
        Some(Ok(handle as Arc<dyn ManagedChildHandle>))
    }));
    let result = recovered(recovery_for(&store, Arc::new(harness.manager)).resume_paused_runs(PARENT_SESSION_ID));
    assert_eq!(result.nodes[0].task_id, Some(owned.task_id));
    assert!(result.nodes.iter().all(|node| node.state == DagNodeState::Completed));
    assert_eq!(harness.in_process.started_count(), 1);
    assert_eq!(harness.in_process.specs()[0].prompt, "do fresh");
}

#[test]
fn two_recovery_managers_observe_one_live_claim_and_one_resume() {
    use std::sync::mpsc;
    use std::time::Duration;
    let store = temp_store();
    // A live resident child of this host (the latest recovery treats a running record without a
    // resident handle as orphaned, so the fixture starts a real resident child).
    let runner = crate::manager::manager_tests::fakes::FakeRunner::new();
    let harness = make_manager(HarnessOptions { in_process: Some(Arc::clone(&runner)), ..HarnessOptions::default() });
    let manager = Arc::new(harness.manager);
    let started = manager.start_owned(&crate::manager::types::ManagerStartSpec {
        prompt: "do active".into(), parent_session_id: PARENT_SESSION_ID.into(),
        root_session_id: Some(ROOT_SESSION_ID.into()), depth: 1, category: Some("quick".into()),
        name: Some("active".into()), run_in_background: true, ..Default::default()
    }, &DagTaskOwner { kind: DagOwnerKind::Dag, run_id: run_id(), node_id: "active".into(), fingerprint: "unused".into() });
    let task_id = match started { OwnedStartResult::Started { task, .. } => task.task_id, other => panic!("task start failed: {other:?}") };
    let handle = runner.wait_handle(&task_id);
    let mut record = base_record(&definition(vec![node("active", &[])]), &[("active", DagNodeState::Running)]);
    record.record.nodes[0].task_id = Some(task_id.clone());
    store.write_checkpoint(&run_id(), &record).unwrap();
    let second_store = Arc::new(create_dag_file_store(&DagStoreConfig { project_dir: store.state_dir.clone(), task: Some(crate::dag::store::DagStoreTaskConfig { state_dir: Some(store.state_dir.clone()), dag: None }) }, DagStoreOptions::default()).unwrap());
    let (claimed_tx, claimed_rx) = mpsc::channel();
    let release = Arc::new(crate::manager::manager_tests::fakes::dag_fake::Gate::default());
    let reattach_release = Arc::clone(&release);
    let first = create_dag_recovery(DagRecoveryOptions {
        store: Arc::clone(&store), task_manager: Arc::clone(&manager), host_pid: Some(101), is_process_alive: Some(Arc::new(|pid| pid == 101 || pid == 202)), is_run_held_in_process: None, now: None, subscriber_ring: None, stop_admission: None, reattach: Some(Arc::new(move |_, _| { claimed_tx.send(()).unwrap(); reattach_release.wait(); })), ancestry: None, node_spawn_policy: None, paused_children: None, resume_credential: None,
    });
    let first_thread = std::thread::spawn(move || first.resume_paused_runs(PARENT_SESSION_ID));
    claimed_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let second = create_dag_recovery(DagRecoveryOptions {
        store: second_store, task_manager: Arc::clone(&manager), host_pid: Some(202), is_process_alive: Some(Arc::new(|pid| pid == 101 || pid == 202)), is_run_held_in_process: None, now: None, subscriber_ring: None, stop_admission: None, reattach: None, ancestry: None, node_spawn_policy: None, paused_children: None, resume_credential: None,
    });
    assert_eq!(second.resume_paused_runs(PARENT_SESSION_ID), vec![DagRecoveryOutcome::Skipped { run_id: run_id(), reason: DagRecoverySkipReason::LiveLease, holder_pid: Some(101) }]);
    release.release();
    handle.complete("done active");
    let result = recovered(first_thread.join().unwrap());
    assert_eq!(result.status, DagRunStatus::Completed);
    assert_eq!(runner.started_count(), 1, "the resident child is reattached, never respawned");
}
