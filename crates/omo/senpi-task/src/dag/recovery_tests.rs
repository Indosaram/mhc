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
        now: None,
        subscriber_ring: None,
        stop_admission: None,
        reattach: None,
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
    let harness = make_manager(HarnessOptions::default());
    let recovery = create_dag_recovery(DagRecoveryOptions {
        store: Arc::clone(&store),
        task_manager: Arc::new(harness.manager),
        host_pid: Some(101),
        is_process_alive: None,
        now: None,
        subscriber_ring: None,
        stop_admission: None,
        reattach: None,
    });

    let outcomes = recovery.resume_paused_runs(PARENT_SESSION_ID);

    assert_eq!(outcomes.len(), 1);
    assert_eq!(
        outcomes[0],
        DagRecoveryOutcome::Skipped {
            run_id: run_id(),
            reason: DagRecoverySkipReason::LiveLease,
        }
    );
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
        now: None,
        subscriber_ring: None,
        stop_admission: None,
        reattach: None,
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
        now: None,
        subscriber_ring: None,
        stop_admission: None,
        reattach: None,
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
        now: Some(Arc::new(move || now_for_closure.load(Ordering::SeqCst))),
        subscriber_ring: None,
        stop_admission: Some(Arc::new(move |_run_id: &DagRunId| {
            order_for_closure.lock().unwrap().push("stop".to_string());
        })),
        reattach: None,
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
        store: Arc::clone(store), task_manager: manager, host_pid: Some(101), is_process_alive: Some(Arc::new(|_| false)), now: None, subscriber_ring: None, stop_admission: None, reattach: None,
    })
}

fn recovered(outcomes: Vec<DagRecoveryOutcome>) -> Box<DagRunRecordV1> {
    assert_eq!(outcomes.len(), 1);
    match outcomes.into_iter().next().unwrap() {
        DagRecoveryOutcome::Resumed { record, .. } => record,
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
    let harness = make_manager(HarnessOptions::default());
    let task = durable_task(&harness, "active", TaskStatus::Running);
    let mut record = base_record(&definition(vec![node("active", &[])]), &[("active", DagNodeState::Running)]);
    record.record.nodes[0].task_id = Some(task.task_id.clone());
    store.write_checkpoint(&run_id(), &record).unwrap();
    let second_store = Arc::new(create_dag_file_store(&DagStoreConfig { project_dir: store.state_dir.clone(), task: Some(crate::dag::store::DagStoreTaskConfig { state_dir: Some(store.state_dir.clone()), dag: None }) }, DagStoreOptions::default()).unwrap());
    let manager = Arc::new(harness.manager);
    let (claimed_tx, claimed_rx) = mpsc::channel();
    let release = Arc::new(crate::manager::manager_tests::fakes::dag_fake::Gate::default());
    let reattach_release = Arc::clone(&release);
    let first = create_dag_recovery(DagRecoveryOptions {
        store: Arc::clone(&store), task_manager: Arc::clone(&manager), host_pid: Some(101), is_process_alive: Some(Arc::new(|pid| pid == 101 || pid == 202)), now: None, subscriber_ring: None, stop_admission: None, reattach: Some(Arc::new(move |_, _| { claimed_tx.send(()).unwrap(); reattach_release.wait(); })),
    });
    let first_thread = std::thread::spawn(move || first.resume_paused_runs(PARENT_SESSION_ID));
    claimed_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let second = create_dag_recovery(DagRecoveryOptions {
        store: second_store, task_manager: Arc::clone(&manager), host_pid: Some(202), is_process_alive: Some(Arc::new(|pid| pid == 101 || pid == 202)), now: None, subscriber_ring: None, stop_admission: None, reattach: None,
    });
    assert_eq!(second.resume_paused_runs(PARENT_SESSION_ID), vec![DagRecoveryOutcome::Skipped { run_id: run_id(), reason: DagRecoverySkipReason::LiveLease }]);
    let mut completed = task; completed.status = TaskStatus::Completed; completed.final_response = Some("done active".into());
    harness.store.replace(&completed).unwrap();
    release.release();
    let result = recovered(first_thread.join().unwrap());
    assert_eq!(result.status, DagRunStatus::Completed);
    assert_eq!(harness.in_process.started_count(), 0);
}
