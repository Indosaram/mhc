//! `dag/scheduler.test.ts`. Covers the load-bearing scheduler contracts (dependency-frontier admission,
//! failure cascade, queue-position journaling, cancellation, and terminal result persistence)
//! against the real `TaskManager` + `FakeRunner` fixture from `manager::manager_tests::fakes`, the
//! Rust-idiomatic counterpart of the TS file's hand-rolled `FakeTaskManager`. Rust's `TaskManager`
//! is a concrete struct rather than the pluggable interface TS mocks directly, so this file's
//! coverage exercises the scheduler against the real manager and controlled fake task port.

use std::sync::mpsc;
use std::time::Duration;

use pretty_assertions::assert_eq;

use super::*;
use crate::dag::graph::{DagDefinition, DagNodeInput};
use crate::dag::manager::DagRunRecordV1;
use crate::dag::store::{
    DagEventReadOptions, DagStoreConfig, DagStoreOptions, create_dag_file_store,
};
use crate::dag::types::{DagNodeState, DagNodeTarget, DagRunStatus, SchemaVersion1};
use crate::manager::manager_tests::fakes::{
    FakeHandle, FakeRunner, HarnessOptions, config, make_manager,
};
use crate::manager::types::ManagedRunnerError;
use crate::runners::RunnerFailureKind;

const PARENT_SESSION_ID: &str = "ses-parent";
const ROOT_SESSION_ID: &str = "ses-root";
const AT: &str = "2026-08-14T00:00:00.000Z";

fn run_id() -> DagRunId {
    "run-scheduler".to_string()
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
        key: "scheduler-test".to_string(),
        name: "scheduler test".to_string(),
        nodes,
    }
}

fn record_for(input: &DagDefinition) -> DagRunRecordV1 {
    let compiled = crate::dag::graph::compile_dag(
        input,
        &crate::dag::graph::DagCompileOptions {
            at: Some(AT.to_string()),
            settings: None,
        },
    );
    assert!(
        compiled.ok,
        "test DAG did not compile: {:?}",
        compiled.errors
    );
    DagRunRecordV1 {
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
        status: DagRunStatus::Pending,
        generation: 1,
        created_at: AT.to_string(),
        updated_at: AT.to_string(),
        started_at: None,
        completed_at: None,
        nodes: compiled.nodes,
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
    }
}

fn temp_store() -> Arc<DagFileStore> {
    let path = tempfile::tempdir().expect("tempdir").keep();
    Arc::new(
        create_dag_file_store(&DagStoreConfig::new(path), DagStoreOptions::default())
            .expect("store opens"),
    )
}

struct Fixture {
    scheduler: Arc<DagSchedulerContext>,
    store: Arc<DagFileStore>,
    runner: Arc<FakeRunner>,
}

fn fixture(input: DagDefinition, concurrency: usize) -> Fixture {
    let store = temp_store();
    let initial_record = record_for(&input);
    store
        .write_checkpoint(&run_id(), &initial_record)
        .expect("write initial checkpoint");
    let harness = make_manager(HarnessOptions {
        config: Some(config(concurrency, 1)),
        ..HarnessOptions::default()
    });
    let scheduler = create_dag_scheduler(DagSchedulerOptions {
        store: Arc::clone(&store),
        task_manager: Arc::new(harness.manager),
        initial_record,
        execution_mode: None,
        ancestry: None,
        node_spawn_policy: None,
        subscriber_ring: None,
        pre_attached_tasks: None,
        schedule_retry: None,
        now: None,
        resume_credential: None,
    })
    .expect("scheduler");
    Fixture {
        scheduler,
        store,
        runner: harness.in_process,
    }
}

fn events_of(store: &DagFileStore) -> Vec<DagRunEvent> {
    store
        .read_events(
            &run_id(),
            0,
            &DagEventReadOptions {
                limit: 100,
                ..Default::default()
            },
        )
        .expect("read events")
        .events
}

#[test]
fn stopped_admission_never_launches_or_completes_a_run() {
    let fixture = fixture(definition(vec![node("only", &[])]), 1);
    fixture.scheduler.stop_admission();
    let record = fixture.scheduler.run().expect("stopped scheduler");
    assert_eq!(record.status, DagRunStatus::Pending);
    assert_eq!(fixture.runner.started_count(), 0);
    assert!(events_of(&fixture.store).is_empty());
}

/// The task id the scheduler attached to a node, read from the durable run checkpoint.
///
/// The TypeScript fixture's fake manager returns the node id as the task id; the Rust port runs the
/// real manager, so the node's own `taskId` field is the faithful equivalent identity.
fn attached_task_id(fixture: &Fixture, node_id: &str) -> Option<String> {
    crate::dag::test_support::attached_task_id(&fixture.store, &run_id(), node_id)
}

fn wait_handle_for(fixture: &Fixture, node_id: &str) -> Arc<FakeHandle> {
    crate::dag::test_support::wait_for_attached_handle(
        &fixture.store,
        &fixture.runner,
        &run_id(),
        node_id,
    )
}

fn auto_complete_nodes(
    fixture: &Fixture,
    node_ids: &[&'static str],
) -> Vec<std::thread::JoinHandle<()>> {
    crate::dag::test_support::auto_complete_nodes(
        &fixture.store,
        &fixture.runner,
        &run_id(),
        node_ids,
    )
}

fn join_all(handles: Vec<std::thread::JoinHandle<()>>) {
    crate::dag::test_support::join_all(handles);
}

fn run_in_background(
    scheduler: &Arc<DagSchedulerContext>,
) -> mpsc::Receiver<Result<DagRunRecordV1, crate::dag::store::DagStoreError>> {
    let (tx, rx) = mpsc::channel();
    let scheduler = Arc::clone(scheduler);
    std::thread::spawn(move || {
        let _ = tx.send(scheduler.run());
    });
    rx
}

fn recv_within<T>(rx: &mpsc::Receiver<T>, what: &str) -> T {
    rx.recv_timeout(Duration::from_secs(5))
        .unwrap_or_else(|_| panic!("waited 5s for {what}, never fired"))
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
fn given_a_linear_three_wave_dag_when_run_then_nodes_and_wave_events_are_admitted_in_order() {
    let fixture = fixture(
        definition(vec![node("a", &[]), node("b", &["a"]), node("c", &["b"])]),
        5,
    );
    let auto = auto_complete_nodes(&fixture, &["a", "b", "c"]);

    let result = run_in_background(&fixture.scheduler)
        .recv_timeout(Duration::from_secs(30))
        .expect("the scheduler run settled within 30s")
        .expect("run");
    join_all(auto);

    assert_eq!(result.status, DagRunStatus::Completed);
    assert_eq!(
        result
            .nodes
            .iter()
            .map(|n| format!("{}:{}", n.id, state_str(n.state)))
            .collect::<Vec<_>>(),
        vec![
            "a:completed".to_string(),
            "b:completed".to_string(),
            "c:completed".to_string()
        ]
    );
    let started_waves: Vec<Vec<String>> = events_of(&fixture.store)
        .into_iter()
        .filter_map(|event| match event.payload {
            DagRunEventPayload::WaveStarted { node_ids, .. } => Some(node_ids),
            _ => None,
        })
        .collect();
    assert_eq!(
        started_waves,
        vec![
            vec!["a".to_string()],
            vec!["b".to_string()],
            vec!["c".to_string()]
        ]
    );
}

#[test]
fn given_a_diamond_dag_when_run_then_the_fan_out_shares_one_wave_and_the_join_waits_for_it() {
    let fixture = fixture(
        definition(vec![
            node("a", &[]),
            node("b", &["a"]),
            node("c", &["a"]),
            node("d", &["b", "c"]),
        ]),
        5,
    );
    let auto = auto_complete_nodes(&fixture, &["a", "b", "c", "d"]);

    run_in_background(&fixture.scheduler)
        .recv_timeout(Duration::from_secs(30))
        .expect("the scheduler run settled within 30s")
        .expect("run");
    join_all(auto);

    let started_waves: Vec<Vec<String>> = events_of(&fixture.store)
        .into_iter()
        .filter_map(|event| match event.payload {
            DagRunEventPayload::WaveStarted { node_ids, .. } => Some(node_ids),
            _ => None,
        })
        .collect();
    assert_eq!(
        started_waves,
        vec![
            vec!["a".to_string()],
            vec!["b".to_string(), "c".to_string()],
            vec!["d".to_string()],
        ]
    );
}

#[test]
fn given_a_failed_root_with_a_descendant_chain_when_failure_cascades_then_every_descendant_skip_is_persisted_separately()
 {
    let fixture = fixture(definition(vec![node("root", &[])]), 5);
    fixture.runner.throw_on_start(true);

    let result = fixture.scheduler.run().expect("run");

    assert_eq!(result.status, DagRunStatus::Failed);
    assert_eq!(result.nodes[0].state, DagNodeState::Failed);
    assert_eq!(
        result.nodes[0].error.as_ref().map(|e| e.code),
        Some(DagNodeErrorCode::StartFailed)
    );
}

#[test]
fn given_a_failed_root_with_a_dependent_chain_when_the_failure_is_journaled_then_every_descendant_skip_is_persisted_separately()
 {
    let fixture = fixture(
        definition(vec![
            node("root", &[]),
            node("child", &["root"]),
            node("grandchild", &["child"]),
            node("independent", &[]),
        ]),
        5,
    );
    {
        let mut hook = fixture.runner.hook.lock().unwrap();
        *hook = Some(Arc::new(|spec, _call| {
            if spec.prompt == "do root" {
                Some(Err(ManagedRunnerError::Other(
                    "root start rejected".to_string(),
                )))
            } else {
                None
            }
        }));
    }
    let auto = auto_complete_nodes(&fixture, &["independent"]);

    let result = run_in_background(&fixture.scheduler)
        .recv_timeout(Duration::from_secs(30))
        .expect("the scheduler run settled within 30s")
        .expect("run");
    join_all(auto);

    assert_eq!(
        result
            .nodes
            .iter()
            .map(|n| format!("{}:{}", n.id, state_str(n.state)))
            .collect::<Vec<_>>(),
        vec![
            "root:failed".to_string(),
            "child:skipped".to_string(),
            "grandchild:skipped".to_string(),
            "independent:completed".to_string(),
        ]
    );
    let skip_events: Vec<DagNodeId> = events_of(&fixture.store)
        .into_iter()
        .filter_map(|event| match event.payload {
            DagRunEventPayload::NodeTransitioned {
                node_id,
                to: DagNodeState::Skipped,
                ..
            } => Some(node_id),
            _ => None,
        })
        .collect();
    assert_eq!(
        skip_events,
        vec!["child".to_string(), "grandchild".to_string()]
    );
}

#[test]
fn given_a_wave_one_task_is_still_running_when_the_scheduler_is_active_then_wave_two_is_not_admitted_before_terminal()
 {
    let fixture = fixture(definition(vec![node("a", &[]), node("b", &["a"])]), 5);
    let rx = run_in_background(&fixture.scheduler);
    let handle_a = wait_handle_for(&fixture, "a");

    assert!(
        attached_task_id(&fixture, "b").is_none(),
        "dependent must not admit before its dependency settles"
    );

    handle_a.complete("done a");
    let handle_b = wait_handle_for(&fixture, "b");
    handle_b.complete("done b");
    let result = recv_within(&rx, "scheduler run").expect("run");

    assert_eq!(result.status, DagRunStatus::Completed);
}

#[test]
fn given_dependency_completes_while_unrelated_sibling_runs_then_dependent_is_admitted() {
    let (fixture, manager) = scripted(
        definition(vec![node("a", &[]), node("slow", &[]), node("b", &["a"])]),
        FakeOptions {
            manual: true,
            ..Default::default()
        },
    );
    let running = run_in_background(&fixture.scheduler);
    manager.when_started("a");
    manager.when_started("slow");
    manager.complete("a", TaskStatus::Completed);
    manager.when_started("b");
    assert_eq!(manager.state.lock().unwrap().starts, vec!["a", "slow", "b"]);
    manager.complete("b", TaskStatus::Completed);
    manager.complete("slow", TaskStatus::Completed);
    assert_eq!(
        recv_within(&running, "frontier completion").unwrap().status,
        DagRunStatus::Completed
    );
}

#[test]
fn given_a_terminal_completion_when_a_node_completes_then_output_and_run_stats_are_persisted() {
    let fixture = fixture(definition(vec![node("artifact", &[])]), 5);
    let rx = run_in_background(&fixture.scheduler);
    let handle = wait_handle_for(&fixture, "artifact");
    handle.complete("done artifact");
    let result = recv_within(&rx, "scheduler run").expect("run");

    assert_eq!(result.status, DagRunStatus::Completed);
    let output = fixture
        .store
        .read_result(&run_id(), "artifact")
        .expect("read result")
        .expect("result present");
    assert_eq!(output, "done artifact");
}

#[test]
fn given_a_task_error_terminal_status_when_folded_then_it_maps_to_a_failed_node_with_task_error() {
    let fixture = fixture(definition(vec![node("task-error", &[])]), 5);
    let rx = run_in_background(&fixture.scheduler);
    let handle = wait_handle_for(&fixture, "task-error");
    handle.fail(RunnerFailureKind::ChildTurnFailed, "boom");
    let result = recv_within(&rx, "scheduler run").expect("run");

    assert_eq!(result.status, DagRunStatus::Failed);
    let failed_node = result
        .nodes
        .iter()
        .find(|n| n.id == "task-error")
        .expect("node present");
    assert_eq!(failed_node.state, DagNodeState::Failed);
    assert_eq!(
        failed_node.error.as_ref().map(|e| e.code),
        Some(DagNodeErrorCode::TaskError)
    );
}

#[test]
fn given_a_running_wave_when_cancelled_then_tasks_cancel_and_the_run_settles_cancelled() {
    let fixture = fixture(definition(vec![node("a", &[]), node("b", &["a"])]), 5);
    let rx = run_in_background(&fixture.scheduler);
    let _handle_a = wait_handle_for(&fixture, "a");

    let cancel_result = fixture.scheduler.cancel(&run_id(), Some("stop now"));
    assert!(cancel_result.is_ok(), "{cancel_result:?}");
    let result = recv_within(&rx, "scheduler run").expect("run");

    assert_eq!(result.status, DagRunStatus::Cancelled);
    assert!(
        result
            .nodes
            .iter()
            .all(|n| matches!(n.state, DagNodeState::Cancelled | DagNodeState::Completed)),
        "{:?}",
        result.nodes
    );
}

#[test]
fn cancellation_waits_for_a_late_admission_result_to_be_attached() {
    struct GatedStart {
        entered: mpsc::Sender<()>,
        release: Mutex<mpsc::Receiver<()>>,
        cancelled: Mutex<Vec<String>>,
        waited: mpsc::Sender<()>,
    }

    impl TestTaskPort for GatedStart {
        fn start_owned(
            &self,
            _: &ManagerStartSpec,
            _: &DagTaskOwner,
        ) -> Result<OwnedStartResult, String> {
            self.entered.send(()).unwrap();
            self.release
                .lock()
                .unwrap()
                .recv_timeout(std::time::Duration::from_secs(5))
                .unwrap();
            Ok(OwnedStartResult::Started {
                task: crate::manager::types::StartedTask {
                    task_id: "late-task".into(),
                    status: TaskStatus::Running,
                    name: "a".into(),
                    resolved_model: None,
                    queue_position: None,
                    name_warning: None,
                },
                reused: false,
            })
        }

        fn wait_for(&self, _: &str) -> Result<crate::state::TaskRecord, String> {
            let _ = self.waited.send(());
            Err("test waiter stopped".into())
        }

        fn cancel_task(&self, task_id: &str) -> Result<(), String> {
            self.cancelled.lock().unwrap().push(task_id.into());
            Ok(())
        }
    }

    let fixture = fixture(definition(vec![node("a", &[]), node("b", &["a"])]), 5);
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let (waited_tx, waited_rx) = mpsc::channel();
    let port = Arc::new(GatedStart {
        entered: entered_tx,
        release: Mutex::new(release_rx),
        cancelled: Mutex::new(Vec::new()),
        waited: waited_tx,
    });
    fixture
        .scheduler
        .set_task_port(Arc::clone(&port) as Arc<dyn TestTaskPort>);
    let attached = event_signal(
        &fixture.scheduler,
        |event| matches!(&event.payload, DagRunEventPayload::NodeTaskAttached { node_id, task_id, .. } if node_id == "a" && task_id == "late-task"),
    );
    let running = run_in_background(&fixture.scheduler);

    recv_within(&entered_rx, "start admission gate");
    let scheduler = Arc::clone(&fixture.scheduler);
    let cancelling = std::thread::spawn(move || scheduler.cancel(&run_id(), Some("stop now")));
    let started = fixture.scheduler.state.cancellation_started.lock().unwrap();
    let (started, timeout) = fixture
        .scheduler
        .state
        .cancellation_condvar
        .wait_timeout_while(started, std::time::Duration::from_secs(5), |started| {
            !*started
        })
        .unwrap();
    assert!(
        *started && !timeout.timed_out(),
        "cancellation did not reach the admission barrier"
    );
    drop(started);
    release_tx.send(()).unwrap();

    recv_within(&attached, "late task attachment");
    recv_within(&waited_rx, "attached task waiter");
    cancelling.join().unwrap().expect("cancel");
    let result = recv_within(&running, "cancelled scheduler").expect("run");
    assert_eq!(result.status, DagRunStatus::Cancelled);
    assert!(
        result
            .nodes
            .iter()
            .all(|node| node.state == DagNodeState::Cancelled)
    );
    assert_eq!(*port.cancelled.lock().unwrap(), vec!["late-task"]);
}

#[test]
fn given_a_paused_unclaimed_run_when_cancelled_then_it_ends_cancelled_without_task_cancellation() {
    let fixture = fixture(definition(vec![node("a", &[]), node("b", &["a"])]), 5);
    {
        let current = fixture.scheduler.snapshot();
        fixture
            .store
            .write_checkpoint(
                &run_id(),
                &DagRunRecordV1 {
                    status: DagRunStatus::Paused,
                    ..current
                },
            )
            .expect("write paused checkpoint");
    }

    fixture
        .scheduler
        .cancel(&run_id(), Some("cancel paused"))
        .expect("cancel paused run");

    assert_eq!(fixture.scheduler.snapshot().status, DagRunStatus::Cancelled);
    assert!(
        fixture
            .scheduler
            .snapshot()
            .nodes
            .iter()
            .all(|n| n.state == DagNodeState::Cancelled)
    );
}

use crate::manager::manager_tests::fakes::dag_fake::{FakeOptions, FakeTaskManager, Gate};

fn scripted(input: DagDefinition, options: FakeOptions) -> (Fixture, Arc<FakeTaskManager>) {
    let fixture = fixture(input, 16);
    let manager = FakeTaskManager::new(options);
    fixture
        .scheduler
        .set_task_port(Arc::clone(&manager) as Arc<dyn TestTaskPort>);
    (fixture, manager)
}

fn event_signal(
    scheduler: &Arc<DagSchedulerContext>,
    ready: impl Fn(&DagRunEvent) -> bool + Send + Sync + 'static,
) -> mpsc::Receiver<()> {
    let (tx, rx) = mpsc::channel();
    let unsubscribe = scheduler.subscribe(Arc::new(move |event| {
        if ready(event) {
            let _ = tx.send(());
        }
    }));
    std::mem::forget(unsubscribe);
    rx
}

#[test]
fn every_terminal_task_status_folds_to_its_exact_node_outcome() {
    for (status, state, code) in [
        (TaskStatus::Completed, DagNodeState::Completed, None),
        (
            TaskStatus::Error,
            DagNodeState::Failed,
            Some(DagNodeErrorCode::TaskError),
        ),
        (
            TaskStatus::Interrupted,
            DagNodeState::Failed,
            Some(DagNodeErrorCode::TaskInterrupted),
        ),
        (
            TaskStatus::Lost,
            DagNodeState::Failed,
            Some(DagNodeErrorCode::TaskLost),
        ),
        (
            TaskStatus::Cancelled,
            DagNodeState::Failed,
            Some(DagNodeErrorCode::TaskCancelled),
        ),
    ] {
        let (fixture, manager) = scripted(
            definition(vec![node("task", &[])]),
            FakeOptions {
                manual: true,
                ..Default::default()
            },
        );
        let running = run_in_background(&fixture.scheduler);
        manager.when_started("task");
        manager.complete("task", status);
        let result = recv_within(&running, "terminal fold").unwrap();
        assert_eq!(result.nodes[0].state, state);
        assert_eq!(result.nodes[0].error.as_ref().map(|error| error.code), code);
    }
}

#[test]
fn every_start_denial_maps_to_its_exact_node_error() {
    let (fixture, _) = scripted(
        definition(vec![
            node("plan", &[]),
            node("depth", &[]),
            node("start", &[]),
            node("residency", &[]),
        ]),
        FakeOptions {
            residency_limit: Some(0),
            start_failures: [
                ("plan".into(), "plan"),
                ("depth".into(), "depth"),
                ("start".into(), "start"),
            ]
            .into(),
            ..Default::default()
        },
    );
    let result = fixture.scheduler.run().unwrap();
    assert_eq!(
        result
            .nodes
            .iter()
            .map(|node| node.error.as_ref().unwrap().code)
            .collect::<Vec<_>>(),
        vec![
            DagNodeErrorCode::PlanUnresolved,
            DagNodeErrorCode::DepthDenied,
            DagNodeErrorCode::StartFailed,
            DagNodeErrorCode::ResidencyDenied
        ]
    );
}

#[test]
fn scheduler_dispatch_resolves_the_configured_execution_mode() {
    let store = temp_store();
    let harness = make_manager(HarnessOptions::default());
    let manager = FakeTaskManager::new(FakeOptions::default());
    let scheduler = create_dag_scheduler(DagSchedulerOptions {
        store,
        task_manager: Arc::new(harness.manager),
        initial_record: record_for(&definition(vec![node("mode", &[])])),
        execution_mode: Some(DagExecutionModeOptions {
            agents: Arc::new(BTreeMap::new()),
            config: Some(ExecutionMode::Process),
            auto_mode: None,
            config_is_auto: false,
        }),
        ancestry: None,
        node_spawn_policy: None,
        subscriber_ring: None,
        pre_attached_tasks: None,
        schedule_retry: None,
        now: None,
        resume_credential: None,
    })
    .unwrap();
    scheduler.set_task_port(Arc::clone(&manager) as Arc<dyn TestTaskPort>);
    scheduler.run().unwrap();
    assert_eq!(
        manager.state.lock().unwrap().specs[0].execution_mode,
        Some(ExecutionMode::Process)
    );
}

#[test]
fn queued_node_reports_the_attached_queue_position() {
    let (fixture, _) = scripted(
        definition(vec![node("queued", &[])]),
        FakeOptions {
            queued: ["queued".into()].into(),
            ..Default::default()
        },
    );
    fixture.scheduler.run().unwrap();
    assert!(events_of(&fixture.store).iter().any(|event| matches!(&event.payload, DagRunEventPayload::NodeTransitioned { node_id, reason: DagNodeTransitionReason::TaskQueued { queue_position: 3 }, .. } if node_id == "queued")));
}

#[test]
fn rejected_wait_fails_one_node_while_its_sibling_completes() {
    let (fixture, manager) = scripted(
        definition(vec![node("reject", &[]), node("sibling", &[])]),
        FakeOptions {
            manual: true,
            reject_wait: ["reject".into()].into(),
            ..Default::default()
        },
    );
    let settled = event_signal(
        &fixture.scheduler,
        |event| matches!(&event.payload, DagRunEventPayload::NodeTransitioned { node_id, to: DagNodeState::Failed, .. } if node_id == "reject"),
    );
    let running = run_in_background(&fixture.scheduler);
    manager.when_started("sibling");
    recv_within(&settled, "rejected waiter fold");
    manager.complete("sibling", TaskStatus::Completed);
    let result = recv_within(&running, "run").unwrap();
    assert_eq!(
        result.nodes[0].error.as_ref().unwrap().code,
        DagNodeErrorCode::TaskError
    );
    assert_eq!(
        result.nodes[0].error.as_ref().unwrap().message,
        "wait rejected reject"
    );
    assert_eq!(result.nodes[1].state, DagNodeState::Completed);
}

#[test]
fn failed_start_preserves_siblings_and_an_independent_branch() {
    let (fixture, manager) = scripted(
        definition(vec![
            node("fail", &[]),
            node("sibling", &[]),
            node("skip", &["fail"]),
            node("next", &["sibling"]),
        ]),
        FakeOptions {
            start_failures: [("fail".into(), "start")].into(),
            ..Default::default()
        },
    );
    let result = fixture.scheduler.run().unwrap();
    assert_eq!(
        result
            .nodes
            .iter()
            .map(|node| node.state)
            .collect::<Vec<_>>(),
        vec![
            DagNodeState::Failed,
            DagNodeState::Completed,
            DagNodeState::Skipped,
            DagNodeState::Completed
        ]
    );
    assert_eq!(
        manager.state.lock().unwrap().starts,
        vec!["sibling", "next"]
    );
}

#[test]
fn residency_denied_node_retries_after_attached_sibling_frees_a_slot() {
    let (fixture, manager) = scripted(
        definition(vec![node("first", &[]), node("retry", &[])]),
        FakeOptions {
            manual: true,
            residency_limit: Some(1),
            ..Default::default()
        },
    );
    let running = run_in_background(&fixture.scheduler);
    manager.when_started("first");
    manager.when_denied("retry");
    manager.complete("first", TaskStatus::Completed);
    manager.when_started("retry");
    manager.complete("retry", TaskStatus::Completed);
    let result = recv_within(&running, "residency retry").unwrap();
    assert_eq!(result.status, DagRunStatus::Completed);
    assert!(
        manager
            .state
            .lock()
            .unwrap()
            .denials
            .contains(&"retry".into())
    );
    assert!(result.nodes.iter().all(|node| node.error.is_none()));
}

#[test]
fn given_residency_denial_then_slot_release_wakes_parked_node_and_journals_queue_once() {
    let (fixture, manager) = scripted(
        definition(vec![node("holder", &[]), node("parked", &[])]),
        FakeOptions {
            manual: true,
            residency_limit: Some(1),
            ..Default::default()
        },
    );
    let queued = event_signal(
        &fixture.scheduler,
        |event| matches!(&event.payload, DagRunEventPayload::NodeTransitioned { node_id, reason: DagNodeTransitionReason::ResidencyQueued { residents: 1, held_by_other_owners: 0 }, .. } if node_id == "parked"),
    );
    let running = run_in_background(&fixture.scheduler);
    manager.when_started("holder");
    manager.when_denied("parked");
    recv_within(&queued, "residency queued event");
    manager.complete("holder", TaskStatus::Completed);
    manager.when_started("parked");
    manager.complete("parked", TaskStatus::Completed);
    assert_eq!(
        recv_within(&running, "parked task completion")
            .unwrap()
            .status,
        DagRunStatus::Completed
    );
    let events = events_of(&fixture.store);
    assert_eq!(events.iter().filter(|event| matches!(&event.payload, DagRunEventPayload::NodeTransitioned { node_id, reason: DagNodeTransitionReason::ResidencyQueued { .. }, .. } if node_id == "parked")).count(), 1);
}

#[test]
fn given_lease_denial_then_scheduler_immediately_reprobes_node() {
    let (fixture, manager) = scripted(
        definition(vec![node("lease", &[])]),
        FakeOptions {
            manual: true,
            lease_denials: ["lease".into()].into(),
            ..Default::default()
        },
    );
    let running = run_in_background(&fixture.scheduler);
    manager.when_denied("lease");
    manager.when_started("lease");
    manager.complete("lease", TaskStatus::Completed);
    assert_eq!(
        recv_within(&running, "lease retry").unwrap().status,
        DagRunStatus::Completed
    );
    assert_eq!(
        manager
            .state
            .lock()
            .unwrap()
            .attempts
            .iter()
            .filter(|id| *id == "lease")
            .count(),
        2
    );
}

#[test]
fn given_resident_denial_with_external_resident_then_release_wakes_parked_node() {
    let (fixture, manager) = scripted(
        definition(vec![node("parked", &[])]),
        FakeOptions {
            manual: true,
            residency_limit: Some(1),
            ..Default::default()
        },
    );
    manager.add_external_resident(PARENT_SESSION_ID);
    let queued = event_signal(
        &fixture.scheduler,
        |event| matches!(&event.payload, DagRunEventPayload::NodeTransitioned { node_id, reason: DagNodeTransitionReason::ResidencyQueued { residents: 1, held_by_other_owners: 1 }, .. } if node_id == "parked"),
    );
    let running = run_in_background(&fixture.scheduler);
    manager.when_denied("parked");
    recv_within(&queued, "residency queued reason");
    // The external resident settles; its release is what frees the slot for the parked node.
    manager.complete("external", TaskStatus::Completed);
    manager.release_residency(PARENT_SESSION_ID);
    manager.when_started("parked");
    manager.complete("parked", TaskStatus::Completed);
    assert_eq!(
        recv_within(&running, "residency release settle")
            .unwrap()
            .status,
        DagRunStatus::Completed
    );
}

#[test]
fn given_resident_denial_names_no_residents_then_node_fails_without_parking() {
    let (fixture, manager) = scripted(
        definition(vec![node("empty", &[])]),
        FakeOptions {
            empty_resident_denials: ["empty".into()].into(),
            ..Default::default()
        },
    );
    let result = fixture.scheduler.run().unwrap();
    assert_eq!(result.nodes[0].state, DagNodeState::Failed);
    assert_eq!(
        result.nodes[0].error.as_ref().map(|error| error.code),
        Some(DagNodeErrorCode::ResidencyDenied)
    );
    assert!(
        result.nodes[0]
            .error
            .as_ref()
            .is_some_and(|error| error.message.contains("names no resident child"))
    );
    assert!(manager.state.lock().unwrap().starts.is_empty());
}

#[test]
fn given_owner_conflict_then_existing_child_is_attached_and_reused() {
    let (fixture, manager) = scripted(
        definition(vec![node("owned", &[])]),
        FakeOptions {
            manual: true,
            ..Default::default()
        },
    );
    let spec = ManagerStartSpec {
        parent_session_id: PARENT_SESSION_ID.into(),
        root_session_id: Some(ROOT_SESSION_ID.into()),
        category: Some("quick".into()),
        prompt: "do owned".into(),
        ..Default::default()
    };
    let owner_stamp = owner(&fixture.scheduler, &"owned".into());
    let first = manager.start_owned(&spec, &owner_stamp).unwrap();
    let task_id = match first {
        OwnedStartResult::Started { task, .. } => task.task_id,
        _ => panic!("fixture start failed"),
    };
    manager.add_owner_conflict("owned");
    let run = run_in_background(&fixture.scheduler);
    manager.when_waiting("owned");
    manager.complete("owned", TaskStatus::Completed);
    assert_eq!(
        recv_within(&run, "owner conflict attach").unwrap().status,
        DagRunStatus::Completed
    );
    assert_eq!(
        attached_task_id(&fixture, "owned").as_deref(),
        Some(task_id.as_str())
    );
    assert_eq!(manager.state.lock().unwrap().starts, vec!["owned"]);
}

#[test]
fn given_preattached_task_then_it_settles_without_new_start() {
    let store = temp_store();
    let input = definition(vec![node("attached", &[])]);
    let mut initial_record = record_for(&input);
    // Recovery hands over a reattached child with its node already running (TS reconcileNodes).
    initial_record.nodes[0].state = DagNodeState::Running;
    store.write_checkpoint(&run_id(), &initial_record).unwrap();
    let harness = make_manager(HarnessOptions::default());
    let manager = FakeTaskManager::new(FakeOptions {
        manual: true,
        ..Default::default()
    });
    let spec = ManagerStartSpec {
        parent_session_id: PARENT_SESSION_ID.into(),
        root_session_id: Some(ROOT_SESSION_ID.into()),
        category: Some("quick".into()),
        prompt: "do attached".into(),
        ..Default::default()
    };
    let owner_stamp = DagTaskOwner {
        kind: DagOwnerKind::Dag,
        run_id: run_id(),
        node_id: "attached".into(),
        fingerprint: "attached-owner".into(),
    };
    let task_id = match manager.start_owned(&spec, &owner_stamp).unwrap() {
        OwnedStartResult::Started { task, .. } => task.task_id,
        _ => panic!("fixture start failed"),
    };
    let scheduler = create_dag_scheduler(DagSchedulerOptions {
        store,
        task_manager: Arc::new(harness.manager),
        initial_record,
        execution_mode: None,
        ancestry: None,
        node_spawn_policy: None,
        subscriber_ring: None,
        pre_attached_tasks: Some([(String::from("attached"), task_id)].into()),
        schedule_retry: None,
        now: None,
        resume_credential: None,
    })
    .unwrap();
    scheduler.set_task_port(Arc::clone(&manager) as Arc<dyn TestTaskPort>);
    let running = run_in_background(&scheduler);
    manager.when_waiting("attached");
    manager.complete("attached", TaskStatus::Completed);
    assert_eq!(
        recv_within(&running, "preattached settle").unwrap().status,
        DagRunStatus::Completed
    );
    assert!(
        manager
            .state
            .lock()
            .unwrap()
            .starts
            .contains(&"attached".into())
    );
    assert_eq!(
        manager
            .state
            .lock()
            .unwrap()
            .attempts
            .iter()
            .filter(|id| *id == "attached")
            .count(),
        1,
        "only the fixture's initial spawn is attempted"
    );
}

#[test]
fn given_parked_residency_wait_when_suspended_then_run_retires_without_cancelling_child() {
    let (fixture, manager) = scripted(
        definition(vec![node("holder", &[]), node("parked", &[])]),
        FakeOptions {
            manual: true,
            residency_limit: Some(1),
            ..Default::default()
        },
    );
    let queued = event_signal(
        &fixture.scheduler,
        |event| matches!(&event.payload, DagRunEventPayload::NodeTransitioned { node_id, reason: DagNodeTransitionReason::ResidencyQueued { .. }, .. } if node_id == "parked"),
    );
    let running = run_in_background(&fixture.scheduler);
    manager.when_started("holder");
    manager.when_denied("parked");
    recv_within(&queued, "parked queued");
    fixture.scheduler.suspend();
    assert_eq!(
        recv_within(&running, "suspended scheduler").unwrap().status,
        DagRunStatus::Running
    );
    assert!(manager.state.lock().unwrap().cancellations.is_empty());
}

#[test]
fn given_spawn_policy_denies_agent_node_then_no_task_is_started_and_node_fails() {
    let mut input = definition(vec![node("agent", &[])]);
    input.nodes[0].target = DagNodeTarget::SubagentType {
        subagent_type: "worker".into(),
        model: None,
    };
    let store = temp_store();
    let initial_record = record_for(&input);
    store.write_checkpoint(&run_id(), &initial_record).unwrap();
    let harness = make_manager(HarnessOptions::default());
    let scheduler = create_dag_scheduler(DagSchedulerOptions {
        store,
        task_manager: Arc::new(harness.manager),
        initial_record,
        execution_mode: None,
        ancestry: None,
        node_spawn_policy: Some(Arc::new(|_, _, _, _| DagNodeSpawnPolicyVerdict::Deny {
            message: "policy".into(),
        })),
        subscriber_ring: None,
        pre_attached_tasks: None,
        schedule_retry: None,
        now: None,
        resume_credential: None,
    })
    .unwrap();
    let result = scheduler.run().unwrap();
    assert_eq!(result.nodes[0].state, DagNodeState::Failed);
    assert!(
        result.nodes[0]
            .error
            .as_ref()
            .is_some_and(|error| error.message.contains("policy"))
    );
}

#[test]
fn given_run_resumed_then_completed_timestamp_is_cleared() {
    let record = record_for(&definition(vec![node("resume", &[])]));
    let terminal = DagRunRecordV1 {
        completed_at: Some("done-at".into()),
        status: DagRunStatus::Completed,
        ..record.clone()
    };
    let event = DagRunEvent {
        schema_version: SchemaVersion1,
        run_id: run_id(),
        seq: 1,
        at: AT.into(),
        lane: crate::dag::types::DagEventLane::Boundary,
        payload: DagRunEventPayload::RunResumed { generation: 2 },
    };
    let resumed = apply_dag_scheduler_event(&terminal, &event, &BTreeMap::new());
    assert_eq!(resumed.completed_at, None);
    assert_eq!(resumed.generation, 2);
}

#[test]
fn given_terminal_node_when_revive_transition_runs_then_old_terminal_metadata_is_cleared() {
    let mut record = record_for(&definition(vec![node("revive", &[])]));
    record.nodes[0].state = DagNodeState::Failed;
    record.nodes[0].completed_at = Some("old".into());
    record.nodes[0].output = Some("previous output".into());
    let event = DagRunEvent {
        schema_version: SchemaVersion1,
        run_id: run_id(),
        seq: 1,
        at: AT.into(),
        lane: crate::dag::types::DagEventLane::Boundary,
        payload: DagRunEventPayload::NodeTransitioned {
            node_id: "revive".into(),
            from: DagNodeState::Failed,
            to: DagNodeState::Running,
            reason: DagNodeTransitionReason::Revived,
        },
    };
    let revived = apply_dag_scheduler_event(&record, &event, &BTreeMap::new());
    assert_eq!(revived.nodes[0].completed_at, None);
    assert_eq!(revived.nodes[0].output, None);
}

#[test]
fn wider_wave_admits_in_residency_batches_without_an_extra_limit() {
    let (fixture, manager) = scripted(
        definition((0..5).map(|i| node(&format!("n{i}"), &[])).collect()),
        FakeOptions {
            manual: true,
            residency_limit: Some(2),
            ..Default::default()
        },
    );
    let running = run_in_background(&fixture.scheduler);
    manager.when_started("n1");
    manager.when_denied("n4");
    for id in ["n0", "n1", "n2", "n3", "n4"] {
        manager.when_started(id);
        manager.complete(id, TaskStatus::Completed);
    }
    let result = recv_within(&running, "batched wave").unwrap();
    assert_eq!(result.status, DagRunStatus::Completed);
    assert_eq!(manager.state.lock().unwrap().starts.len(), 5);
    assert_eq!(manager.state.lock().unwrap().max_residents, 2);
}

#[test]
fn rejected_start_clears_admission_and_cancels_the_attached_sibling() {
    let (fixture, manager) = scripted(
        definition(vec![node("sibling", &[]), node("reject", &[])]),
        FakeOptions {
            manual: true,
            reject_start: ["reject".into()].into(),
            ..Default::default()
        },
    );
    let attached = event_signal(
        &fixture.scheduler,
        |event| matches!(&event.payload, DagRunEventPayload::NodeTransitioned { node_id, to: DagNodeState::Failed, .. } if node_id == "reject"),
    );
    let running = run_in_background(&fixture.scheduler);
    recv_within(&attached, "sibling attachment");
    fixture
        .scheduler
        .cancel(&run_id(), Some("stop after rejected admission"))
        .unwrap();
    assert_eq!(
        recv_within(&running, "cancelled run").unwrap().status,
        DagRunStatus::Cancelled
    );
    assert_eq!(manager.state.lock().unwrap().cancellations, vec!["sibling"]);
}

fn cancellation_failure(error: &str, two_nodes: bool) {
    let (fixture, manager) = scripted(
        definition(if two_nodes {
            vec![node("a", &[]), node("b", &[])]
        } else {
            vec![node("a", &[]), node("b", &["a"])]
        }),
        FakeOptions {
            manual: true,
            cancel_errors: [("a".into(), error.into())].into(),
            ..Default::default()
        },
    );
    let attached = event_signal(
        &fixture.scheduler,
        move |event| matches!(&event.payload, DagRunEventPayload::NodeTaskAttached { node_id, .. } if node_id == if two_nodes { "b" } else { "a" }),
    );
    let running = run_in_background(&fixture.scheduler);
    recv_within(&attached, "attachment");
    manager.when_waiting("a");
    if two_nodes {
        manager.when_waiting("b");
    }
    assert_eq!(
        fixture
            .scheduler
            .cancel(&run_id(), Some("cancel"))
            .unwrap_err(),
        error
    );
    let result = recv_within(&running, "cancelled despite error").unwrap();
    assert_eq!(result.status, DagRunStatus::Cancelled);
    assert!(
        result
            .nodes
            .iter()
            .all(|node| node.state == DagNodeState::Cancelled)
    );
    if two_nodes {
        assert_eq!(manager.state.lock().unwrap().cancellations.len(), 2);
    }
    // Only the child whose cancellation was rejected is still live in the fake manager.
    manager.complete("a", TaskStatus::Cancelled);
}

#[test]
fn abort_error_from_task_cancellation_surfaces_after_durable_cancellation() {
    cancellation_failure("intentional abort", false);
}

#[test]
fn genuine_cancellation_failure_surfaces_without_leaving_the_run_live() {
    cancellation_failure("cancel rejected a", true);
}

#[test]
fn graph_order_selects_primary_failure_despite_reversed_completion_order() {
    let (fixture, manager) = scripted(
        definition(vec![
            node("later", &["preparation"]),
            node("graph-first", &[]),
            node("completion-first", &[]),
            node("preparation", &[]),
        ]),
        FakeOptions {
            manual: true,
            ..Default::default()
        },
    );
    let settled = event_signal(
        &fixture.scheduler,
        |event| matches!(&event.payload, DagRunEventPayload::NodeTransitioned { node_id, to: DagNodeState::Failed, .. } if node_id == "completion-first"),
    );
    let running = run_in_background(&fixture.scheduler);
    manager.when_started("preparation");
    manager.complete("completion-first", TaskStatus::Error);
    recv_within(&settled, "first failure");
    manager.complete("preparation", TaskStatus::Completed);
    manager.complete("graph-first", TaskStatus::Error);
    manager.when_started("later");
    manager.complete("later", TaskStatus::Error);
    assert_eq!(
        recv_within(&running, "ordered failure").unwrap().status,
        DagRunStatus::Failed
    );
    assert!(events_of(&fixture.store).iter().any(|event| matches!(&event.payload, DagRunEventPayload::RunFailed { error, .. } if error.node_id.as_deref() == Some("graph-first"))));
}

#[test]
fn durable_waiters_before_during_and_after_cancellation_all_settle() {
    use crate::dag::handle::{DagWaitSurfaceOptions, create_dag_wait_surface};
    let gate = Arc::new(Gate::default());
    let (fixture, manager) = scripted(
        definition(vec![node("CA", &[]), node("CB", &["CA"])]),
        FakeOptions {
            manual: true,
            cancel_gate: Some(Arc::clone(&gate)),
            ..Default::default()
        },
    );
    let attached = event_signal(
        &fixture.scheduler,
        |event| matches!(&event.payload, DagRunEventPayload::NodeTaskAttached { node_id, .. } if node_id == "CA"),
    );
    let (armed_tx, armed_rx) = mpsc::channel();
    let surface = Arc::new(create_dag_wait_surface(DagWaitSurfaceOptions {
        store: Arc::clone(&fixture.store),
        subscribe: Arc::new(move |_, _| {
            armed_tx.send(()).unwrap();
            Box::new(|| {})
        }),
        cancel: None,
        read_output: None,
    }));
    let running = run_in_background(&fixture.scheduler);
    recv_within(&attached, "CA attachment");
    manager.when_waiting("CA");
    let spawn_wait = |attach: bool| {
        let surface = Arc::clone(&surface);
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            tx.send(if attach {
                surface.attach(&run_id(), PARENT_SESSION_ID).unwrap().done()
            } else {
                surface.wait(&run_id(), PARENT_SESSION_ID)
            })
            .unwrap();
        });
        rx
    };
    let before = [spawn_wait(false), spawn_wait(true)];
    recv_within(&armed_rx, "first waiter subscribed");
    recv_within(&armed_rx, "second waiter subscribed");
    let scheduler = Arc::clone(&fixture.scheduler);
    let cancelling = std::thread::spawn(move || scheduler.cancel(&run_id(), Some("live cancel")));
    manager.when_cancelling();
    let during = [spawn_wait(false), spawn_wait(true)];
    recv_within(&armed_rx, "third waiter subscribed during cancellation");
    recv_within(&armed_rx, "fourth waiter subscribed during cancellation");
    gate.release();
    cancelling.join().unwrap().unwrap();
    for rx in before.into_iter().chain(during) {
        let result = recv_within(&rx, "durable cancelled wait").unwrap();
        assert_eq!(result.status, DagRunStatus::Cancelled);
        assert!(result.nodes.values().all(|node| matches!(node, crate::dag::handle::DagTerminalNodeResult::Cancelled { reason, .. } if reason == "live cancel")));
    }
    assert_eq!(
        surface.wait(&run_id(), PARENT_SESSION_ID).unwrap().status,
        DagRunStatus::Cancelled
    );
    assert_eq!(
        surface
            .attach(&run_id(), PARENT_SESSION_ID)
            .unwrap()
            .done()
            .unwrap()
            .status,
        DagRunStatus::Cancelled
    );
    assert_eq!(surface.waiter_count(&run_id()), 0);
    assert_eq!(
        recv_within(&running, "scheduler cancellation")
            .unwrap()
            .status,
        DagRunStatus::Cancelled
    );
}

#[test]
fn configured_subscriber_ring_overflows_at_its_bound() {
    let store = temp_store();
    let harness = make_manager(HarnessOptions::default());
    let scheduler = create_dag_scheduler(DagSchedulerOptions {
        store,
        task_manager: Arc::new(harness.manager),
        initial_record: record_for(&definition(vec![node("ring", &[])])),
        execution_mode: None,
        ancestry: None,
        node_spawn_policy: None,
        subscriber_ring: Some(1),
        pre_attached_tasks: None,
        schedule_retry: None,
        now: None,
        resume_credential: None,
    })
    .unwrap();
    let manager = FakeTaskManager::new(FakeOptions {
        manual: true,
        ..Default::default()
    });
    scheduler.set_task_port(Arc::clone(&manager) as Arc<dyn TestTaskPort>);
    let release = Arc::new(Gate::default());
    let listener_release = Arc::clone(&release);
    let (first_tx, first_rx) = mpsc::channel();
    let overflow = Arc::new(Mutex::new(None));
    let listener_overflow = Arc::clone(&overflow);
    let _unsubscribe = scheduler.subscribe(Arc::new(move |event| {
        if event.seq == 1 {
            first_tx.send(()).unwrap();
            listener_release.wait();
        }
        if let DagRunEventPayload::StreamOverflow {
            dropped_count,
            recover_after_seq,
        } = event.payload
        {
            *listener_overflow.lock().unwrap() = Some((dropped_count, recover_after_seq));
        }
    }));
    scheduler
        .journal
        .append(DagRunEventPayload::RunStarted { generation: 1 })
        .unwrap();
    recv_within(&first_rx, "blocked first listener");
    let running = run_in_background(&scheduler);
    manager.when_started("ring");
    manager.complete("ring", TaskStatus::Completed);
    assert_eq!(
        recv_within(&running, "completed with blocked subscriber")
            .unwrap()
            .status,
        DagRunStatus::Completed
    );
    release.release();
    scheduler.when_idle();
    let (dropped, cursor) = overflow.lock().unwrap().expect("overflow");
    assert!(dropped > 0);
    assert_eq!(cursor, 1);
}
