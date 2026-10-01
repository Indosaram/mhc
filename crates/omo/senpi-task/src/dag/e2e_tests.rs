//! `dag/e2e-happy.test.ts`, `dag/e2e-failure.test.ts`. One end-to-end smoke test proving the
//! assembled `DagManager` + `DagScheduler` + `DagWaitSurface` + real `TaskManager` agree on the
//! happy path (linear three-node run, wave-ordered events, durable artifacts) and one proving a
//! mid-chain failure cascades to `skipped` descendants end to end. Rust's `TaskManager` is a
//! concrete struct rather than the pluggable interface TS scripts directly with `ScriptedRunner`,
//! so this file's coverage is narrower than the pinned 30-case (12+18) TS suite: the remaining
//! diamond/mixed-route/residency/cancellation-under-load end-to-end permutations are tracked as an
//! N/A gap in the parity row, each already covered at the unit level by
//! `manager_tests.rs`/`scheduler_tests.rs`/`handle_tests.rs`.

use std::sync::mpsc;
use std::time::Duration;

use pretty_assertions::assert_eq;

use crate::dag::graph::{DagDefinition, DagNodeInput};
use crate::dag::handle::{DagRunResult, DagSubscribe, DagWaitSurfaceOptions, create_dag_wait_surface};
use crate::dag::manager::{DagManagerOptions, DagStartParams, create_dag_manager};
use crate::dag::scheduler::{DagSchedulerContext, DagSchedulerOptions, create_dag_scheduler};
use crate::dag::store::{DagEventReadOptions, DagStoreConfig, DagStoreOptions, create_dag_file_store};
use crate::dag::types::{DagNodeState, DagNodeTarget, DagRunEventPayload, DagRunId, DagRunStatus};
use crate::manager::manager_tests::fakes::{HarnessOptions, config, make_manager};
use std::sync::Arc;

const PARENT_SESSION_ID: &str = "session-e2e-parent";
const ROOT_SESSION_ID: &str = "session-e2e-root";

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

fn definition(key: &str, nodes: Vec<DagNodeInput>) -> DagDefinition {
    DagDefinition {
        key: key.to_string(),
        name: key.replace('-', " "),
        nodes,
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

fn recv_within<T>(rx: &mpsc::Receiver<T>, what: &str) -> T {
    rx.recv_timeout(Duration::from_secs(5))
        .unwrap_or_else(|_| panic!("waited 5s for {what}, never fired"))
}

#[test]
fn given_a_linear_three_node_definition_when_the_real_engine_runs_then_every_event_output_snapshot_and_artifact_is_wave_ordered()
 {
    let project = tempfile::tempdir().expect("tempdir").keep();
    let store = Arc::new(
        create_dag_file_store(&DagStoreConfig::new(&project), DagStoreOptions::default())
            .expect("store opens"),
    );
    let harness = make_manager(HarnessOptions {
        config: Some(config(16, 1)),
        ..HarnessOptions::default()
    });
    let task_manager = Arc::new(harness.manager);
    let dag_manager = create_dag_manager(DagManagerOptions {
        store: Arc::clone(&store),
        new_run_id: None,
        now: None,
        materialize_skills: None,
        settings: None,
    });

    let input = definition("linear-three", vec![node("plan", &[]), node("build", &["plan"]), node("review", &["build"])]);
    let started = dag_manager
        .start(DagStartParams {
            definition: input,
            parent_session_id: PARENT_SESSION_ID.to_string(),
            root_session_id: ROOT_SESSION_ID.to_string(),
        })
        .expect("start");
    let run_id = started.snapshot.run_id.clone();

    let scheduler: Arc<DagSchedulerContext> = create_dag_scheduler(DagSchedulerOptions {
        store: Arc::clone(&store),
        task_manager: Arc::clone(&task_manager),
        initial_record: dag_manager.record(&run_id, PARENT_SESSION_ID).expect("record"),
        execution_mode_agents: None,
        execution_mode_config: None,
        ancestry_depth: None,
        subscriber_ring: None,
        now: None,
    })
    .expect("scheduler");

    let subscribe_run_id = run_id.clone();
    let subscribe_scheduler = Arc::clone(&scheduler);
    let subscribe: DagSubscribe = Arc::new(move |subscribed_run_id: &DagRunId, listener| {
        if subscribed_run_id == &subscribe_run_id {
            subscribe_scheduler.subscribe(Arc::new(move |_event| listener()))
        } else {
            Box::new(|| {})
        }
    });
    let wait_surface = create_dag_wait_surface(DagWaitSurfaceOptions {
        store: Arc::clone(&store),
        subscribe,
        cancel: None,
        read_output: None,
    });

    let (run_tx, run_rx) = mpsc::channel();
    let run_scheduler = Arc::clone(&scheduler);
    std::thread::spawn(move || {
        let _ = run_tx.send(run_scheduler.run());
    });

    let runner = Arc::clone(&harness.in_process);
    let store_for_completions = Arc::clone(&store);
    let run_id_for_completions = run_id.clone();
    let complete_thread = std::thread::spawn(move || {
        for node_id in ["plan", "build", "review"] {
            let handle = crate::dag::test_support::wait_for_attached_handle(
                &store_for_completions,
                &runner,
                &run_id_for_completions,
                node_id,
            );
            handle.complete(&format!("output:{node_id}"));
        }
    });

    let result: DagRunResult = wait_surface.wait(&run_id, PARENT_SESSION_ID).expect("wait ok");
    complete_thread.join().expect("completions");
    recv_within(&run_rx, "scheduler run").expect("run");

    assert_eq!(result.status, DagRunStatus::Completed);
    assert_eq!(result.snapshot.counts.total, 3);
    assert_eq!(result.snapshot.counts.completed, 3);
    for (node_id, expected_output) in [("plan", "output:plan"), ("build", "output:build"), ("review", "output:review")] {
        let crate::dag::handle::DagTerminalNodeResult::Completed { output, .. } = &result.nodes[node_id] else {
            panic!("expected {node_id} completed");
        };
        assert_eq!(output, expected_output);
    }

    let events = store
        .read_events(&run_id, 0, &DagEventReadOptions { limit: 256, ..Default::default() })
        .expect("read events")
        .events;
    let wave_starts: Vec<Vec<String>> = events
        .iter()
        .filter_map(|event| match &event.payload {
            DagRunEventPayload::WaveStarted { node_ids, .. } => Some(node_ids.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        wave_starts,
        vec![vec!["plan".to_string()], vec!["build".to_string()], vec!["review".to_string()]]
    );
    assert!(matches!(events.last().map(|e| &e.payload), Some(DagRunEventPayload::RunCompleted { .. })));

    assert!(store.paths.run(&run_id).is_file());
    assert!(store.paths.key(PARENT_SESSION_ID, "linear-three").is_file());
    assert!(store.paths.event(&run_id).is_file());
    for node_id in ["plan", "build", "review"] {
        let result_path = store.paths.result(&run_id, node_id);
        assert!(result_path.is_file());
        assert_eq!(
            std::fs::read_to_string(&result_path).expect("read artifact"),
            format!("output:{node_id}")
        );
    }
}

#[test]
fn given_a_failed_root_when_the_real_engine_runs_then_the_dependent_chain_is_skipped_and_the_run_ends_failed()
 {
    let project = tempfile::tempdir().expect("tempdir").keep();
    let store = Arc::new(
        create_dag_file_store(&DagStoreConfig::new(&project), DagStoreOptions::default())
            .expect("store opens"),
    );
    let harness = make_manager(HarnessOptions {
        config: Some(config(16, 1)),
        ..HarnessOptions::default()
    });
    let task_manager = Arc::new(harness.manager);
    let dag_manager = create_dag_manager(DagManagerOptions {
        store: Arc::clone(&store),
        new_run_id: None,
        now: None,
        materialize_skills: None,
        settings: None,
    });
    harness.in_process.throw_on_start(true);

    let input = definition("linear-failure", vec![node("root", &[]), node("child", &["root"])]);
    let started = dag_manager
        .start(DagStartParams {
            definition: input,
            parent_session_id: PARENT_SESSION_ID.to_string(),
            root_session_id: ROOT_SESSION_ID.to_string(),
        })
        .expect("start");
    let run_id = started.snapshot.run_id.clone();

    let scheduler = create_dag_scheduler(DagSchedulerOptions {
        store: Arc::clone(&store),
        task_manager: Arc::clone(&task_manager),
        initial_record: dag_manager.record(&run_id, PARENT_SESSION_ID).expect("record"),
        execution_mode_agents: None,
        execution_mode_config: None,
        ancestry_depth: None,
        subscriber_ring: None,
        now: None,
    })
    .expect("scheduler");

    let result = scheduler.run().expect("run");

    assert_eq!(result.status, DagRunStatus::Failed);
    assert_eq!(
        result
            .nodes
            .iter()
            .map(|n| format!("{}:{}", n.id, state_str(n.state)))
            .collect::<Vec<_>>(),
        vec!["root:failed".to_string(), "child:skipped".to_string()]
    );
}
