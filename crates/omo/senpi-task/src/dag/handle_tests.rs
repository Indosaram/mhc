//! `dag/handle.test.ts`.

use std::sync::mpsc;
use std::time::Duration;

use pretty_assertions::assert_eq;

use super::*;
use crate::dag::journal::{DagJournalOptions, create_dag_journal};
use crate::dag::manager::{DagPersistedDefinition, DagPersistedNode, DagPersistedNodeTarget, DagRunRecordV1};
use crate::dag::store::{DagStoreConfig, DagStoreOptions, create_dag_file_store};
use crate::dag::types::{DagBottleneck, DagDiagnostic, DagEdge, DagNodeState, DagRoute, DagRunEvent, DagWave, SchemaVersion1};

const PARENT_SESSION_ID: &str = "ses_parent";
const OTHER_SESSION_ID: &str = "ses_other";
const ROOT_SESSION_ID: &str = "ses_root";
const AT: &str = "2026-01-01T00:00:00.000Z";

type CancelCalls = Arc<Mutex<Vec<(DagRunId, Option<String>)>>>;

fn run_id() -> DagRunId {
    "run-wait".to_string()
}

fn temp_store() -> Arc<DagFileStore> {
    let path = tempfile::tempdir().expect("tempdir").keep();
    Arc::new(
        create_dag_file_store(&DagStoreConfig::new(path), DagStoreOptions::default())
            .expect("store opens"),
    )
}

struct NodeSpec {
    id: &'static str,
    state: DagNodeState,
    task_id: Option<&'static str>,
    depends_on: Vec<DagNodeId>,
    error: Option<DagNodeError>,
    run_stats: Option<TaskRunStats>,
    completed_at: Option<&'static str>,
}

fn node(id: &'static str) -> NodeSpec {
    NodeSpec {
        id,
        state: DagNodeState::Pending,
        task_id: None,
        depends_on: Vec::new(),
        error: None,
        run_stats: None,
        completed_at: None,
    }
}

fn build_node(spec: NodeSpec) -> DagNode {
    DagNode {
        id: spec.id.to_string(),
        label: None,
        prompt: format!("do {}", spec.id),
        route: DagRoute::Category {
            category: "quick".to_string(),
        },
        depends_on: spec.depends_on,
        state: spec.state,
        task_id: spec.task_id.map(str::to_string),
        attempt: 0,
        error: spec.error,
        run_stats: spec.run_stats,
        created_at: AT.to_string(),
        started_at: None,
        completed_at: spec.completed_at.map(str::to_string),
    }
}

fn record(nodes: Vec<DagNode>, status: DagRunStatus) -> DagRunRecordV1 {
    DagRunRecordV1 {
        schema_version: SchemaVersion1,
        checkpoint_seq: 0,
        run_id: run_id(),
        run_key: "release-plan".to_string(),
        name: "release plan".to_string(),
        parent_session_id: PARENT_SESSION_ID.to_string(),
        root_session_id: ROOT_SESSION_ID.to_string(),
        definition_fingerprint: "fp-1".to_string(),
        definition: DagPersistedDefinition {
            key: "release-plan".to_string(),
            name: "release plan".to_string(),
            nodes: nodes
                .iter()
                .map(|entry| DagPersistedNode {
                    id: entry.id.clone(),
                    prompt: entry.prompt.clone(),
                    target: DagPersistedNodeTarget::Category {
                        category: "quick".to_string(),
                    },
                    label: None,
                    depends_on: None,
                    task_summary: None,
                    description: None,
                    load_skills: None,
                    effective_prompt: entry.prompt.clone(),
                })
                .collect(),
        },
        status,
        generation: 1,
        created_at: AT.to_string(),
        updated_at: AT.to_string(),
        started_at: None,
        completed_at: None,
        waves: vec![DagWave {
            index: 0,
            node_ids: nodes.iter().map(|entry| entry.id.clone()).collect(),
        }],
        critical_path: Vec::new(),
        bottlenecks: Vec::<DagBottleneck>::new(),
        diagnostics: Vec::<DagDiagnostic>::new(),
        edges: Vec::<DagEdge>::new(),
        nodes,
    }
}

fn counts_of(nodes: &[DagNode]) -> DagNodeCounts {
    let mut counts = DagNodeCounts {
        total: nodes.len(),
        ..DagNodeCounts::default()
    };
    for entry in nodes {
        match entry.state {
            DagNodeState::Pending => counts.pending += 1,
            DagNodeState::Blocked => counts.blocked += 1,
            DagNodeState::Scheduled => counts.scheduled += 1,
            DagNodeState::Running => counts.running += 1,
            DagNodeState::Completed => counts.completed += 1,
            DagNodeState::Failed => counts.failed += 1,
            DagNodeState::Cancelled => counts.cancelled += 1,
            DagNodeState::Skipped => counts.skipped += 1,
        }
    }
    counts
}

fn apply_event(checkpoint: &DagRunRecordV1, event: &DagRunEvent) -> DagRunRecordV1 {
    match &event.payload {
        DagRunEventPayload::RunCompleted { .. } => DagRunRecordV1 {
            status: DagRunStatus::Completed,
            completed_at: Some(event.at.clone()),
            ..checkpoint.clone()
        },
        DagRunEventPayload::RunFailed { .. } => DagRunRecordV1 {
            status: DagRunStatus::Failed,
            completed_at: Some(event.at.clone()),
            ..checkpoint.clone()
        },
        DagRunEventPayload::RunCancelled { .. } => DagRunRecordV1 {
            status: DagRunStatus::Cancelled,
            completed_at: Some(event.at.clone()),
            ..checkpoint.clone()
        },
        _ => checkpoint.clone(),
    }
}

struct Harness {
    store: Arc<DagFileStore>,
    journal: Arc<crate::dag::journal::DagJournal<DagRunRecordV1>>,
    surface: DagWaitSurface,
}

impl Harness {
    fn settle(&self, nodes: Vec<DagNode>, status: DagRunStatus, reason: Option<&str>) {
        let current = self.journal.snapshot();
        self.store
            .write_checkpoint(&run_id(), &DagRunRecordV1 {
                nodes: nodes.clone(),
                ..current
            })
            .expect("write checkpoint");
        let payload = match status {
            DagRunStatus::Completed => DagRunEventPayload::RunCompleted {
                counts: counts_of(&nodes),
            },
            DagRunStatus::Failed => DagRunEventPayload::RunFailed {
                error: DagNodeError {
                    code: DagNodeErrorCode::TaskError,
                    message: "build failed".to_string(),
                    node_id: Some("build".to_string()),
                    at: AT.to_string(),
                },
                counts: counts_of(&nodes),
            },
            DagRunStatus::Cancelled => DagRunEventPayload::RunCancelled {
                reason: reason.map(str::to_string),
                counts: counts_of(&nodes),
            },
            other => panic!("unsupported settle status {other:?}"),
        };
        self.journal.append(payload).expect("append terminal event");
    }
}

fn fixture(nodes: Vec<DagNode>) -> Harness {
    fixture_with_cancel(nodes, None)
}

fn fixture_with_cancel(nodes: Vec<DagNode>, cancel: Option<DagCancel>) -> Harness {
    let store = temp_store();
    let initial = record(nodes, DagRunStatus::Running);
    store.write_checkpoint(&run_id(), &initial).expect("write initial checkpoint");
    let journal = create_dag_journal(DagJournalOptions {
        store: Arc::clone(&store),
        run_id: run_id(),
        initial_checkpoint: initial,
        apply_event: Arc::new(apply_event),
        subscriber_ring: None,
        now: None,
    })
    .expect("journal");
    let journal_for_subscribe = Arc::clone(&journal);
    let subscribe: DagSubscribe = Arc::new(move |subscribed_run_id, listener| {
        if subscribed_run_id != &run_id() {
            return Box::new(|| {});
        }
        journal_for_subscribe.subscribe(Arc::new(move |_event: &DagRunEvent| listener()))
    });
    let surface = create_dag_wait_surface(DagWaitSurfaceOptions {
        store: Arc::clone(&store),
        subscribe,
        cancel,
        read_output: None,
    });
    Harness { store, journal, surface }
}

/// Blocking `wait` moves to its own thread so the test can settle the run from the main thread,
/// exactly like the TS test's `await Promise.resolve()` interleaving but expressed for a thread
/// that truly blocks instead of yielding a microtask.
fn wait_in_background(surface: &DagWaitSurface, run: DagRunId, session: &str) -> mpsc::Receiver<Result<DagRunResult, DagWaitError>> {
    let (tx, rx) = mpsc::channel();
    let surface = surface.clone();
    let session = session.to_string();
    std::thread::spawn(move || {
        let _ = tx.send(surface.wait(&run, &session));
    });
    rx
}

fn recv_within<T>(rx: &mpsc::Receiver<T>, what: &str) -> T {
    rx.recv_timeout(Duration::from_secs(5))
        .unwrap_or_else(|_| panic!("waited 5s for {what}, never fired"))
}

#[test]
fn given_a_two_node_run_when_both_nodes_complete_then_wait_resolves_with_each_node_output_and_run_stats()
 {
    let running = vec![
        build_node(NodeSpec { state: DagNodeState::Running, task_id: Some("task-plan"), ..node("plan") }),
        build_node(NodeSpec { state: DagNodeState::Running, task_id: Some("task-build"), ..node("build") }),
    ];
    let harness = fixture(running);
    harness.store.write_result(&run_id(), "plan", "plan output").expect("write plan result");
    harness.store.write_result(&run_id(), "build", "build output").expect("write build result");

    let rx = wait_in_background(&harness.surface, run_id(), PARENT_SESSION_ID);
    std::thread::sleep(Duration::from_millis(50));
    harness.settle(
        vec![
            build_node(NodeSpec {
                state: DagNodeState::Completed,
                task_id: Some("task-plan"),
                completed_at: Some(AT),
                run_stats: Some(TaskRunStats {
                    runtime_ms: 10,
                    turns: 2,
                    tool_calls: 1,
                    output_tokens: None,
                    total_tokens: None,
                    generation_ms: None,
                    tokens_per_second: None,
                    cost_usd: None,
                    cache_hit_rate_last: None,
                    cache_hit_rate_run: None,
                }),
                ..node("plan")
            }),
            build_node(NodeSpec {
                state: DagNodeState::Completed,
                task_id: Some("task-build"),
                completed_at: Some(AT),
                ..node("build")
            }),
        ],
        DagRunStatus::Completed,
        None,
    );
    let result = recv_within(&rx, "wait result").expect("wait ok");

    assert_eq!(result.status, DagRunStatus::Completed);
    assert_eq!(result.run_id, run_id());
    assert_eq!(result.snapshot.status, DagRunStatus::Completed);
    let DagTerminalNodeResult::Completed { task_id, output, run_stats } = &result.nodes["plan"] else {
        panic!("expected plan completed");
    };
    assert_eq!(task_id, "task-plan");
    assert_eq!(output, "plan output");
    assert_eq!(run_stats.as_ref().map(|s| s.runtime_ms), Some(10));
    let DagTerminalNodeResult::Completed { task_id, output, .. } = &result.nodes["build"] else {
        panic!("expected build completed");
    };
    assert_eq!(task_id, "task-build");
    assert_eq!(output, "build output");
}

#[test]
fn given_a_failed_node_with_a_dependent_when_the_run_ends_failed_then_wait_resolves_with_the_node_error_and_the_dependency_skip()
 {
    let harness = fixture(vec![
        build_node(NodeSpec { state: DagNodeState::Running, task_id: Some("task-plan"), ..node("plan") }),
        build_node(NodeSpec { state: DagNodeState::Blocked, depends_on: vec!["plan".to_string()], ..node("ship") }),
    ]);

    let rx = wait_in_background(&harness.surface, run_id(), PARENT_SESSION_ID);
    std::thread::sleep(Duration::from_millis(50));
    harness.settle(
        vec![
            build_node(NodeSpec {
                state: DagNodeState::Failed,
                task_id: Some("task-plan"),
                error: Some(DagNodeError {
                    code: DagNodeErrorCode::TaskError,
                    message: "plan crashed".to_string(),
                    node_id: Some("plan".to_string()),
                    at: AT.to_string(),
                }),
                ..node("plan")
            }),
            build_node(NodeSpec {
                state: DagNodeState::Skipped,
                depends_on: vec!["plan".to_string()],
                ..node("ship")
            }),
        ],
        DagRunStatus::Failed,
        None,
    );
    let result = recv_within(&rx, "wait result").expect("wait ok");

    assert_eq!(result.status, DagRunStatus::Failed);
    let DagTerminalNodeResult::Failed { task_id, error } = &result.nodes["plan"] else {
        panic!("expected plan failed");
    };
    assert_eq!(task_id.as_deref(), Some("task-plan"));
    assert_eq!(error.message, "plan crashed");
    let DagTerminalNodeResult::Skipped { dependency_ids } = &result.nodes["ship"] else {
        panic!("expected ship skipped");
    };
    assert_eq!(dependency_ids, &vec!["plan".to_string()]);
}

#[test]
fn given_a_cancelled_run_when_wait_is_pending_then_it_resolves_rather_than_rejects_and_carries_the_cancellation_reason()
 {
    let harness = fixture(vec![
        build_node(NodeSpec { state: DagNodeState::Running, task_id: Some("task-plan"), ..node("plan") }),
        build_node(node("build")),
    ]);

    let rx = wait_in_background(&harness.surface, run_id(), PARENT_SESSION_ID);
    std::thread::sleep(Duration::from_millis(50));
    harness.settle(
        vec![
            build_node(NodeSpec { state: DagNodeState::Cancelled, task_id: Some("task-plan"), ..node("plan") }),
            build_node(NodeSpec { state: DagNodeState::Cancelled, ..node("build") }),
        ],
        DagRunStatus::Cancelled,
        Some("user_requested"),
    );
    let result = recv_within(&rx, "wait result").expect("wait ok");

    assert_eq!(result.status, DagRunStatus::Cancelled);
    let DagTerminalNodeResult::Cancelled { task_id, reason } = &result.nodes["plan"] else {
        panic!("expected plan cancelled");
    };
    assert_eq!(task_id.as_deref(), Some("task-plan"));
    assert_eq!(reason, "user_requested");
    let DagTerminalNodeResult::Cancelled { reason, .. } = &result.nodes["build"] else {
        panic!("expected build cancelled");
    };
    assert_eq!(reason, "user_requested");
}

#[test]
fn given_a_run_that_is_already_terminal_when_wait_is_called_then_it_resolves_without_any_further_journal_event()
 {
    let harness = fixture(vec![build_node(NodeSpec {
        state: DagNodeState::Running,
        task_id: Some("task-plan"),
        ..node("plan")
    })]);
    harness.store.write_result(&run_id(), "plan", "plan output").expect("write result");
    harness.settle(
        vec![build_node(NodeSpec { state: DagNodeState::Completed, task_id: Some("task-plan"), ..node("plan") })],
        DagRunStatus::Completed,
        None,
    );
    let seq_before = harness
        .store
        .read_events(&run_id(), 0, &DagEventReadOptions { limit: 100, ..Default::default() })
        .expect("read events")
        .head_seq;

    let result = harness.surface.wait(&run_id(), PARENT_SESSION_ID).expect("wait ok");

    assert_eq!(result.status, DagRunStatus::Completed);
    let DagTerminalNodeResult::Completed { task_id, output, .. } = &result.nodes["plan"] else {
        panic!("expected plan completed");
    };
    assert_eq!(task_id, "task-plan");
    assert_eq!(output, "plan output");
    assert_eq!(
        harness
            .store
            .read_events(&run_id(), 0, &DagEventReadOptions { limit: 100, ..Default::default() })
            .expect("read events")
            .head_seq,
        seq_before
    );
}

#[test]
fn given_an_attached_handle_when_done_resolves_then_it_equals_the_wait_result_and_snapshot_tracks_the_live_record()
 {
    let harness = fixture(vec![build_node(NodeSpec {
        state: DagNodeState::Running,
        task_id: Some("task-plan"),
        ..node("plan")
    })]);
    harness.store.write_result(&run_id(), "plan", "plan output").expect("write result");
    let handle = harness.surface.attach(&run_id(), PARENT_SESSION_ID).expect("attach");

    assert_eq!(handle.snapshot().expect("snapshot").status, DagRunStatus::Running);
    let done_rx = {
        let (tx, rx) = mpsc::channel();
        let surface = harness.surface.clone();
        let run = run_id();
        std::thread::spawn(move || {
            let handle = surface.attach(&run, PARENT_SESSION_ID).expect("attach for done");
            let _ = tx.send(handle.done());
        });
        rx
    };
    let wait_rx = wait_in_background(&harness.surface, run_id(), PARENT_SESSION_ID);
    std::thread::sleep(Duration::from_millis(50));
    harness.settle(
        vec![build_node(NodeSpec { state: DagNodeState::Completed, task_id: Some("task-plan"), ..node("plan") })],
        DagRunStatus::Completed,
        None,
    );
    let handle_result = recv_within(&done_rx, "handle.done()").expect("done ok");
    let wait_result = recv_within(&wait_rx, "wait result").expect("wait ok");

    assert_eq!(handle.run_id, run_id());
    assert_eq!(handle_result, wait_result);
    assert_eq!(handle.snapshot().expect("snapshot").status, DagRunStatus::Completed);
}

#[test]
fn given_an_attached_handle_when_cancel_is_called_then_the_injected_cancellation_runs_for_the_owned_run_only()
 {
    let cancelled: CancelCalls = Arc::new(Mutex::new(Vec::new()));
    let cancelled_for_closure = Arc::clone(&cancelled);
    let cancel: DagCancel = Arc::new(move |id: &DagRunId, reason: Option<&str>| {
        cancelled_for_closure
            .lock()
            .unwrap()
            .push((id.clone(), reason.map(str::to_string)));
    });
    let harness = fixture_with_cancel(
        vec![build_node(NodeSpec { state: DagNodeState::Running, task_id: Some("task-plan"), ..node("plan") })],
        Some(cancel),
    );

    harness
        .surface
        .attach(&run_id(), PARENT_SESSION_ID)
        .expect("attach")
        .cancel(Some("user_requested"))
        .expect("cancel");

    assert_eq!(
        *cancelled.lock().unwrap(),
        vec![(run_id(), Some("user_requested".to_string()))]
    );
    assert!(harness.surface.attach(&run_id(), OTHER_SESSION_ID).is_err());
}

#[test]
fn given_a_caller_that_abandons_its_wait_when_the_run_later_completes_then_the_run_is_untouched_and_a_fresh_attach_still_returns_the_result()
 {
    let cancelled: Arc<Mutex<Vec<DagRunId>>> = Arc::new(Mutex::new(Vec::new()));
    let cancelled_for_closure = Arc::clone(&cancelled);
    let cancel: DagCancel = Arc::new(move |id: &DagRunId, _reason: Option<&str>| {
        cancelled_for_closure.lock().unwrap().push(id.clone());
    });
    let harness = fixture_with_cancel(
        vec![build_node(NodeSpec { state: DagNodeState::Running, task_id: Some("task-plan"), ..node("plan") })],
        Some(cancel),
    );
    harness.store.write_result(&run_id(), "plan", "plan output").expect("write result");

    let abandoned_rx = wait_in_background(&harness.surface, run_id(), PARENT_SESSION_ID);
    std::thread::sleep(Duration::from_millis(50));
    harness.settle(
        vec![build_node(NodeSpec { state: DagNodeState::Completed, task_id: Some("task-plan"), ..node("plan") })],
        DagRunStatus::Completed,
        None,
    );
    let later = harness
        .surface
        .attach(&run_id(), PARENT_SESSION_ID)
        .expect("attach")
        .done()
        .expect("done ok");

    assert_eq!(*cancelled.lock().unwrap(), Vec::<DagRunId>::new());
    assert_eq!(
        harness
            .store
            .read_checkpoint::<DagRunRecordV1>(&run_id())
            .expect("read checkpoint")
            .map(|record| record.status),
        Some(DagRunStatus::Completed)
    );
    let DagTerminalNodeResult::Completed { task_id, output, .. } = &later.nodes["plan"] else {
        panic!("expected plan completed");
    };
    assert_eq!(task_id, "task-plan");
    assert_eq!(output, "plan output");
    recv_within(&abandoned_rx, "abandoned wait").expect("abandoned settles too");
}

#[test]
fn given_many_waiters_on_one_run_when_every_waiter_detaches_but_one_then_the_survivor_still_resolves_and_no_journal_subscription_leaks()
 {
    let harness = fixture(vec![build_node(NodeSpec {
        state: DagNodeState::Running,
        task_id: Some("task-plan"),
        ..node("plan")
    })]);
    harness.store.write_result(&run_id(), "plan", "plan output").expect("write result");

    let abandoned_first = wait_in_background(&harness.surface, run_id(), PARENT_SESSION_ID);
    let abandoned_second = wait_in_background(&harness.surface, run_id(), PARENT_SESSION_ID);
    let survivor = wait_in_background(&harness.surface, run_id(), PARENT_SESSION_ID);
    std::thread::sleep(Duration::from_millis(50));
    harness.settle(
        vec![build_node(NodeSpec { state: DagNodeState::Completed, task_id: Some("task-plan"), ..node("plan") })],
        DagRunStatus::Completed,
        None,
    );

    assert_eq!(
        recv_within(&survivor, "survivor").expect("survivor ok").status,
        DagRunStatus::Completed
    );
    assert_eq!(harness.surface.waiter_count(&run_id()), 0);
    recv_within(&abandoned_first, "abandoned_first").expect("abandoned_first ok");
    recv_within(&abandoned_second, "abandoned_second").expect("abandoned_second ok");
}

#[test]
fn given_a_run_owned_by_a_dead_session_when_a_new_session_waits_then_it_rejects_with_run_not_owned_instead_of_hanging()
 {
    let harness = fixture(vec![build_node(NodeSpec {
        state: DagNodeState::Running,
        task_id: Some("task-plan"),
        ..node("plan")
    })]);

    let error = harness
        .surface
        .wait(&run_id(), OTHER_SESSION_ID)
        .expect_err("expected run_not_owned");

    assert_eq!(error.code, DagWaitErrorCode::RunNotOwned);
    assert_eq!(error.run_id, Some(run_id()));
}

#[test]
fn given_an_unknown_run_id_when_waited_or_attached_then_run_not_found_is_raised_before_any_subscription()
 {
    let harness = fixture(vec![build_node(NodeSpec {
        state: DagNodeState::Running,
        task_id: Some("task-plan"),
        ..node("plan")
    })]);
    let missing: DagRunId = "run-missing".to_string();

    let error = harness.surface.wait(&missing, PARENT_SESSION_ID).expect_err("expected run_not_found");
    assert_eq!(error.code, DagWaitErrorCode::RunNotFound);
    assert!(harness.surface.attach(&missing, PARENT_SESSION_ID).is_err());
}

#[test]
fn given_malformed_wait_params_when_waited_then_invalid_arguments_is_raised_and_no_run_is_touched()
 {
    let harness = fixture(vec![build_node(NodeSpec {
        state: DagNodeState::Running,
        task_id: Some("task-plan"),
        ..node("plan")
    })]);

    let blank_run = harness.surface.wait(&String::new(), PARENT_SESSION_ID);
    let blank_session = harness.surface.wait(&run_id(), "");

    assert_eq!(
        blank_run.expect_err("expected invalid_arguments").code,
        DagWaitErrorCode::InvalidArguments
    );
    assert_eq!(
        blank_session.expect_err("expected invalid_arguments").code,
        DagWaitErrorCode::InvalidArguments
    );
    assert_eq!(
        harness
            .store
            .read_checkpoint::<DagRunRecordV1>(&run_id())
            .expect("read checkpoint")
            .map(|record| record.status),
        Some(DagRunStatus::Running)
    );
}
