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

struct ScriptedFixture {
    _project: crate::manager::manager_tests::fakes::Project,
    store: Arc<crate::dag::store::DagFileStore>,
    manager: crate::dag::manager::DagManager,
    tasks: Arc<crate::manager::TaskManager>,
    runner: Arc<crate::manager::manager_tests::fakes::FakeRunner>,
}

fn scripted_fixture() -> ScriptedFixture {
    use crate::manager::manager_tests::fakes::{FakeHandle, FakeRunner};
    let runner = FakeRunner::new();
    *runner.hook.lock().unwrap() = Some(Arc::new(|spec, _| {
        let handle = FakeHandle::new(&spec.task_id, None);
        handle.complete(&format!("output:{}", spec.prompt.strip_prefix("do ").unwrap_or(&spec.prompt)));
        Some(Ok(handle as Arc<dyn crate::manager::ManagedChildHandle>))
    }));
    let harness = make_manager(HarnessOptions {
        config: Some(config(16, 1)), in_process: Some(Arc::clone(&runner)), process: Some(Arc::clone(&runner)),
        planner: Some(Arc::new(|spec| {
            let target = spec.category.as_deref().or(spec.subagent_type.as_deref()).unwrap_or("default");
            Ok(crate::manager::types::ResolvedChildPlan { model: format!("scripted/{target}"), category: spec.category.clone(), agent_type: spec.subagent_type.clone(), ..Default::default() })
        })), ..Default::default()
    });
    let store = Arc::new(create_dag_file_store(&DagStoreConfig::new(harness.project.dir.path()), DagStoreOptions::default()).unwrap());
    let manager = create_dag_manager(DagManagerOptions { store: Arc::clone(&store), new_run_id: None, now: None, materialize_skills: None, settings: None });
    ScriptedFixture { _project: harness.project, store, manager, tasks: Arc::new(harness.manager), runner }
}

impl ScriptedFixture {
    fn start(&self, input: DagDefinition) -> crate::dag::manager::DagStartResult {
        self.manager.start(DagStartParams { definition: input, parent_session_id: PARENT_SESSION_ID.into(), root_session_id: ROOT_SESSION_ID.into() }).unwrap()
    }

    fn run(&self, run_id: &DagRunId) -> DagRunResult {
        let scheduler = create_dag_scheduler(DagSchedulerOptions { store: Arc::clone(&self.store), task_manager: Arc::clone(&self.tasks), initial_record: self.manager.record(run_id, PARENT_SESSION_ID).unwrap(), execution_mode_agents: None, execution_mode_config: None, ancestry_depth: None, subscriber_ring: None, now: None }).unwrap();
        scheduler.run().unwrap();
        let wait = create_dag_wait_surface(DagWaitSurfaceOptions { store: Arc::clone(&self.store), subscribe: Arc::new(|_, _| Box::new(|| {})), cancel: None, read_output: None });
        wait.wait(run_id, PARENT_SESSION_ID).unwrap()
    }

    fn events(&self, run_id: &str) -> Vec<crate::dag::types::DagRunEvent> {
        self.store.read_events(run_id, 0, &DagEventReadOptions { limit: 256, ..Default::default() }).unwrap().events
    }
}

fn agent_node(id: &str, dependencies: &[&str], agent: &str) -> DagNodeInput {
    DagNodeInput { target: DagNodeTarget::SubagentType { subagent_type: agent.into(), model: None }, ..node(id, dependencies) }
}

fn assert_scripted_success(fixture: &ScriptedFixture, input: DagDefinition, waves: Vec<Vec<&str>>) -> DagRunResult {
    let started = fixture.start(input.clone());
    let result = fixture.run(&started.snapshot.run_id);
    assert_eq!(result.status, DagRunStatus::Completed);
    assert_eq!(result.snapshot.counts.completed, input.nodes.len());
    let actual_waves = fixture.events(&result.run_id).into_iter().filter_map(|event| match event.payload { DagRunEventPayload::WaveStarted { node_ids, .. } => Some(node_ids), _ => None }).collect::<Vec<_>>();
    assert_eq!(actual_waves, waves);
    let events = fixture.events(&result.run_id);
    for (index, wave) in waves.iter().enumerate() {
        let start = events.iter().position(|event| matches!(event.payload, DagRunEventPayload::WaveStarted { wave_index, .. } if wave_index == index)).unwrap();
        let finish = events.iter().position(|event| matches!(event.payload, DagRunEventPayload::WaveCompleted { wave_index, .. } if wave_index == index)).unwrap();
        for id in wave {
            let attached = events.iter().position(|event| matches!(&event.payload, DagRunEventPayload::NodeTaskAttached { node_id, .. } if node_id == id)).unwrap();
            let completed = events.iter().position(|event| matches!(&event.payload, DagRunEventPayload::NodeTransitioned { node_id, to: DagNodeState::Completed, .. } if node_id == id)).unwrap();
            assert!(start < attached && attached < completed && completed < finish);
        }
        if index + 1 < waves.len() {
            let next = events.iter().position(|event| matches!(event.payload, DagRunEventPayload::WaveStarted { wave_index, .. } if wave_index == index + 1)).unwrap();
            assert!(finish < next);
        }
    }
    for node in &input.nodes {
        assert!(matches!(&result.nodes[&node.id], crate::dag::handle::DagTerminalNodeResult::Completed { output, .. } if output == &format!("output:{}", node.id)));
        assert_eq!(std::fs::read_to_string(fixture.store.paths.result(&result.run_id, &node.id)).unwrap(), format!("output:{}", node.id));
    }
    assert!(fixture.store.paths.run(&result.run_id).is_file());
    assert!(fixture.store.paths.key(PARENT_SESSION_ID, &input.key).is_file());
    assert!(matches!(fixture.events(&result.run_id).last().unwrap().payload, DagRunEventPayload::RunCompleted { .. }));
    result
}

#[test]
fn scripted_diamond_preserves_fanout_and_join_waves_end_to_end() {
    let fixture = scripted_fixture();
    assert_scripted_success(&fixture, definition("diamond", vec![node("root", &[]), agent_node("left", &["root"], "explore"), node("right", &["root"]), agent_node("join", &["left", "right"], "momus")]), vec![vec!["root"], vec!["left", "right"], vec!["join"]]);
}

#[test]
fn scripted_mixed_routes_preserve_models_membership_and_outputs() {
    let fixture = scripted_fixture();
    let mut design = node("design", &["intake"]); design.target = DagNodeTarget::Category("visual-engineering".into());
    let mut budget = node("budget", &["intake"]); budget.target = DagNodeTarget::Category("deep".into());
    let mut docs = node("docs", &["evidence", "budget"]); docs.target = DagNodeTarget::Category("writing".into());
    let input = definition("mixed-eight", vec![node("intake", &[]), agent_node("research", &[], "explore"), design, agent_node("evidence", &["research"], "librarian"), budget, agent_node("build", &["design", "evidence"], "hephaestus"), docs, agent_node("review", &["build", "docs"], "momus")]);
    let result = assert_scripted_success(&fixture, input.clone(), vec![vec!["intake", "research"], vec!["design", "evidence", "budget"], vec!["build", "docs"], vec!["review"]]);
    assert_eq!(result.snapshot.nodes.iter().map(|node| node.id.clone()).collect::<Vec<_>>(), input.nodes.iter().map(|node| node.id.clone()).collect::<Vec<_>>());
    let specs = fixture.runner.specs();
    for ((spec, target), input) in specs.iter().zip(["quick", "explore", "visual-engineering", "librarian", "deep", "hephaestus", "writing", "momus"]).zip(&input.nodes) {
        assert_eq!(spec.model, Some(format!("scripted/{target}")));
        match &input.target {
            DagNodeTarget::Category(category) => {
                assert_eq!(spec.agent_type, None);
                assert!(matches!(&result.snapshot.nodes.iter().find(|node| node.id == input.id).unwrap().route, crate::dag::types::DagRoute::Category { category: actual } if actual == category));
            }
            DagNodeTarget::SubagentType { subagent_type, .. } => {
                assert_eq!(spec.agent_type.as_ref(), Some(subagent_type));
                assert!(matches!(&result.snapshot.nodes.iter().find(|node| node.id == input.id).unwrap().route, crate::dag::types::DagRoute::Agent { agent, .. } if agent == subagent_type));
            }
        }
    }
}

#[test]
fn completed_run_key_reuses_checkpoint_events_and_task_count() {
    let fixture = scripted_fixture();
    let input = definition("restart-once", vec![node("only", &[])]);
    let result = assert_scripted_success(&fixture, input.clone(), vec![vec!["only"]]);
    let events = fixture.events(&result.run_id);
    let second = fixture.start(input);
    assert!(second.reused);
    assert_eq!(second.snapshot.run_id, result.run_id);
    assert_eq!(second.snapshot.status, DagRunStatus::Completed);
    assert_eq!(fixture.events(&result.run_id), events);
    assert_eq!(std::fs::read_dir(&fixture.store.paths.runs).unwrap().count(), 1);
    assert_eq!(fixture.runner.started_count(), 1);
}

#[test]
fn changed_prompt_conflict_preserves_checkpoint_events_and_tasks() {
    let fixture = scripted_fixture();
    let input = definition("conflict", vec![node("only", &[])]);
    let result = assert_scripted_success(&fixture, input.clone(), vec![vec!["only"]]);
    let checkpoint = std::fs::read(fixture.store.paths.run(&result.run_id)).unwrap();
    let events = std::fs::read(fixture.store.paths.event(&result.run_id)).unwrap();
    let mut changed = input; changed.nodes[0].prompt = "changed prompt".into();
    let error = fixture.manager.start(DagStartParams { definition: changed, parent_session_id: PARENT_SESSION_ID.into(), root_session_id: ROOT_SESSION_ID.into() }).unwrap_err();
    assert_eq!(error.code, crate::dag::manager::DagManagerErrorCode::DefinitionConflict);
    assert_eq!(std::fs::read(fixture.store.paths.run(&result.run_id)).unwrap(), checkpoint);
    assert_eq!(std::fs::read(fixture.store.paths.event(&result.run_id)).unwrap(), events);
    assert_eq!(fixture.runner.started_count(), 1);
}

#[test]
fn corrupt_tail_reopens_with_authoritative_events_and_recovery_diagnostic() {
    use std::io::Write;
    let fixture = scripted_fixture();
    let result = assert_scripted_success(&fixture, definition("event-tail", vec![node("done", &[])]), vec![vec!["done"]]);
    let authoritative = fixture.events(&result.run_id);
    std::fs::OpenOptions::new().append(true).open(fixture.store.paths.event(&result.run_id)).unwrap().write_all(b"{\"schemaVersion\":1,\"seq\":999").unwrap();
    let reopened = create_dag_file_store(&crate::dag::store::DagStoreConfig { project_dir: fixture.store.state_dir.clone(), task: Some(crate::dag::store::DagStoreTaskConfig { state_dir: Some(fixture.store.state_dir.clone()), dag: None }) }, DagStoreOptions::default()).unwrap();
    assert_eq!(reopened.read_events(&result.run_id, 0, &DagEventReadOptions { limit: 256, ..Default::default() }).unwrap().events, authoritative);
    assert!(!reopened.diagnostics().is_empty());
    assert!(std::fs::read(reopened.paths.event(&result.run_id)).unwrap().ends_with(b"\n"));
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct MutationCheckpoint {
    schema_version: u8,
    checkpoint_seq: u64,
    applied: Vec<u64>,
}

impl crate::dag::journal::DagJournalCheckpoint for MutationCheckpoint {
    fn checkpoint_seq(&self) -> u64 { self.checkpoint_seq }
    fn with_checkpoint_seq(&self, checkpoint_seq: u64) -> Self { Self { checkpoint_seq, ..self.clone() } }
}

#[test]
fn fsync_mutation_child_process() {
    use std::io::{Read, Write};
    let temporary = tempfile::tempdir().unwrap();
    let child_mode = std::env::var("MAHO_DAG_CRASH_PROJECT").ok();
    let project = child_mode.clone().unwrap_or_else(|| temporary.path().to_string_lossy().into_owned());
    let stop_at: u64 = std::env::var("MAHO_DAG_CRASH_SEQ").map(|value| value.parse().unwrap()).unwrap_or(3);
    let store = create_dag_file_store(&DagStoreConfig::new(project), DagStoreOptions::default()).unwrap();
    let run_id = "dag-kill-mid-mutation";
    store.write_checkpoint(run_id, &MutationCheckpoint { schema_version: 1, checkpoint_seq: 0, applied: vec![] }).unwrap();
    for seq in 1..=stop_at {
        store.append_event(&crate::dag::types::DagRunEvent { schema_version: crate::dag::types::SchemaVersion1, run_id: run_id.into(), seq, at: "2026-08-14T00:00:00.000Z".into(), lane: crate::dag::types::DagEventLane::Boundary, payload: DagRunEventPayload::DiagnosticAdded { diagnostic: crate::dag::types::DagDiagnostic::RunFlag { message: seq.to_string(), at: "2026-08-14T00:00:00.000Z".into() } } }).unwrap();
        if seq == stop_at && child_mode.is_some() {
            println!("ARMED {seq}");
            std::io::stdout().flush().unwrap();
            std::io::stdin().read_exact(&mut [0]).unwrap();
        }
        store.write_checkpoint(run_id, &MutationCheckpoint { schema_version: 1, checkpoint_seq: seq, applied: (1..=seq).collect() }).unwrap();
    }
    let checkpoint: MutationCheckpoint = store.read_checkpoint(run_id).unwrap().unwrap();
    assert_eq!(checkpoint.applied, (1..=stop_at).collect::<Vec<_>>());
}

#[test]
fn sigkill_after_fsynced_wal_leaves_whole_checkpoint_and_replays_every_event() {
    use std::io::{BufRead, BufReader};
    use std::os::unix::process::ExitStatusExt;
    use std::process::{Command, Stdio};
    let project = tempfile::tempdir().unwrap();
    let stop_at = 2 + u64::from(std::process::id() % 7);
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "dag::e2e_tests::fsync_mutation_child_process", "--nocapture"])
        .env("MAHO_DAG_CRASH_PROJECT", project.path())
        .env("MAHO_DAG_CRASH_SEQ", stop_at.to_string())
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::inherit()).spawn().unwrap();
    let stdout = child.stdout.take().unwrap();
    let (armed_tx, armed_rx) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            let line = line.unwrap();
            if let Some(seq) = line.strip_prefix("ARMED ") { armed_tx.send(seq.parse::<u64>().unwrap()).unwrap(); break; }
        }
    });
    assert_eq!(recv_within(&armed_rx, "fsynced mutation boundary"), stop_at);
    child.kill().unwrap();
    assert_eq!(child.wait().unwrap().signal(), Some(9));
    reader.join().unwrap();
    let store = Arc::new(create_dag_file_store(&DagStoreConfig::new(project.path()), DagStoreOptions::default()).unwrap());
    let before: MutationCheckpoint = store.read_checkpoint("dag-kill-mid-mutation").unwrap().unwrap();
    assert_eq!(before.checkpoint_seq, stop_at - 1);
    assert_eq!(before.applied, (1..stop_at).collect::<Vec<_>>());
    let journal = crate::dag::journal::create_dag_journal(crate::dag::journal::DagJournalOptions {
        store: Arc::clone(&store), run_id: "dag-kill-mid-mutation".into(), initial_checkpoint: MutationCheckpoint { schema_version: 1, checkpoint_seq: 0, applied: vec![] }, apply_event: Arc::new(|checkpoint: &MutationCheckpoint, event| { let mut next = checkpoint.clone(); next.applied.push(event.seq); next }), subscriber_ring: None, now: None,
    }).unwrap();
    assert_eq!(journal.snapshot().checkpoint_seq, stop_at);
    assert_eq!(journal.snapshot().applied, (1..=stop_at).collect::<Vec<_>>());
    assert_eq!(store.read_events("dag-kill-mid-mutation", 0, &DagEventReadOptions { limit: 32, ..Default::default() }).unwrap().events.len() as u64, stop_at);
    assert!(std::fs::read_dir(&store.paths.runs).unwrap().all(|entry| !entry.unwrap().file_name().to_string_lossy().ends_with(".tmp")));
}

#[test]
fn retention_prunes_expired_terminal_artifacts_but_preserves_equally_old_live_artifacts() {
    let project = tempfile::tempdir().unwrap();
    let store = create_dag_file_store(&crate::dag::store::DagStoreConfig { project_dir: project.path().into(), task: Some(crate::dag::store::DagStoreTaskConfig { state_dir: None, dag: Some(crate::dag::store::DagSettingsOverrides { retention_days: Some(7), ..Default::default() }) }) }, DagStoreOptions::default()).unwrap();
    let old = "2026-08-01T00:00:00.000Z";
    for (run_id, status) in [("dag-terminal-expired", "completed"), ("dag-live-old", "running")] {
        store.write_checkpoint(run_id, &serde_json::json!({ "schemaVersion": 1, "checkpointSeq": 1, "runId": run_id, "runKey": run_id, "parentSessionId": PARENT_SESSION_ID, "status": status, "completedAt": old, "updatedAt": old, "nodes": [] })).unwrap();
        store.append_event(&crate::dag::types::DagRunEvent { schema_version: crate::dag::types::SchemaVersion1, run_id: run_id.into(), seq: 1, at: old.into(), lane: crate::dag::types::DagEventLane::Boundary, payload: DagRunEventPayload::DiagnosticAdded { diagnostic: crate::dag::types::DagDiagnostic::RunFlag { message: "retention".into(), at: old.into() } } }).unwrap();
        store.write_result(run_id, "node", &format!("result:{run_id}")).unwrap();
        std::fs::create_dir_all(store.paths.root.join("skills")).unwrap();
        std::fs::write(store.paths.root.join("skills").join(format!("{run_id}.json")), serde_json::json!({"schemaVersion":1,"runId":run_id}).to_string()).unwrap();
    }
    let now = chrono::DateTime::parse_from_rfc3339("2026-08-14T00:00:00.000Z").unwrap().timestamp_millis();
    assert_eq!(store.prune_expired(Some(now)).unwrap(), vec!["dag-terminal-expired"]);
    for (run_id, exists) in [("dag-terminal-expired", false), ("dag-live-old", true)] {
        for path in [store.paths.run(run_id), store.paths.event(run_id), store.paths.results.join(run_id), store.paths.root.join("skills").join(format!("{run_id}.json"))] { assert_eq!(path.exists(), exists, "{}", path.display()); }
    }
}

#[test]
fn missing_requested_skill_diagnostic_survives_a_completed_real_run() {
    let mut fixture = scripted_fixture();
    let materialize = crate::dag::skills::create_dag_skill_materializer(crate::dag::skills::DagSkillMaterializerOptions {
        store: Arc::clone(&fixture.store), cwd: fixture._project.cwd(), load_skills: Some(Arc::new(|names, _| crate::tools::task::types::SkillResolution { missing: names.to_vec(), ..Default::default() })), home_dir: None, extra_dirs: vec![],
    });
    fixture.manager = create_dag_manager(DagManagerOptions { store: Arc::clone(&fixture.store), new_run_id: None, now: None, materialize_skills: Some(materialize), settings: None });
    let mut build = node("build", &[]); build.load_skills = Some(vec!["not-installed".into()]);
    let result = assert_scripted_success(&fixture, definition("missing-skill", vec![build]), vec![vec!["build"]]);
    assert!(result.snapshot.diagnostics.iter().any(|diagnostic| matches!(diagnostic, crate::dag::types::DagDiagnostic::MissingSkill { node_id, skill, .. } if node_id == "build" && skill == "not-installed")));
}

#[test]
fn spawn_before_attachment_recovers_owned_task_without_a_second_spawn() {
    use crate::dag::owner::{DagOwnerKind, DagTaskOwner, OwnedStartResult};
    use crate::dag::recovery::{DagRecoveryOptions, RecoverableRecord, create_dag_recovery};
    let fixture = scripted_fixture();
    let started = fixture.start(definition("spawn-before-attach", vec![node("recover-me", &[])]));
    let run_id = started.snapshot.run_id;
    let mut initial = fixture.manager.record(&run_id, PARENT_SESSION_ID).unwrap();
    initial.status = DagRunStatus::Paused; initial.nodes[0].state = DagNodeState::Scheduled;
    fixture.store.write_checkpoint(&run_id, &RecoverableRecord { record: initial.clone(), lease_holder_pid: None, previous_lease_holder_pid: Some(99001) }).unwrap();
    let owned = fixture.tasks.start_owned(&crate::manager::types::ManagerStartSpec { prompt: "do recover-me".into(), parent_session_id: PARENT_SESSION_ID.into(), root_session_id: Some(ROOT_SESSION_ID.into()), depth: 1, category: Some("quick".into()), name: Some("recover-me".into()), run_in_background: true, ..Default::default() }, &DagTaskOwner { kind: DagOwnerKind::Dag, run_id: run_id.clone(), node_id: "recover-me".into(), fingerprint: crate::dag::fingerprint::dag_fingerprint(&serde_json::json!({"definitionFingerprint": initial.definition_fingerprint, "nodeId": "recover-me"})) });
    let OwnedStartResult::Started { task, .. } = owned else { panic!("owned start failed") };
    fixture.tasks.wait_for(&task.task_id, None, Some(Duration::from_secs(5))).unwrap();
    let second = make_manager(HarnessOptions { project: Some(fixture._project.clone()), ..Default::default() });
    let outcomes = create_dag_recovery(DagRecoveryOptions { store: Arc::clone(&fixture.store), task_manager: Arc::new(second.manager), host_pid: Some(202), is_process_alive: Some(Arc::new(|pid| pid == 202)), now: None, subscriber_ring: None, stop_admission: None, reattach: None }).resume_paused_runs(PARENT_SESSION_ID);
    assert_eq!(outcomes.len(), 1);
    let crate::dag::recovery::DagRecoveryOutcome::Resumed { record, .. } = &outcomes[0] else { panic!("not resumed") };
    assert_eq!(record.status, DagRunStatus::Completed);
    assert_eq!(record.nodes[0].task_id.as_deref(), Some(task.task_id.as_str()));
    assert_eq!(fixture.runner.started_count(), 1);
    assert_eq!(second.in_process.started_count(), 0);
}

#[test]
fn reversed_failures_leave_independent_branches_running_and_select_graph_first() {
    use crate::manager::manager_tests::fakes::FakeHandle;
    let fixture = scripted_fixture();
    let handles = Arc::new((std::sync::Mutex::new(std::collections::BTreeMap::<String, Arc<FakeHandle>>::new()), std::sync::Condvar::new()));
    let started_handles = Arc::clone(&handles);
    *fixture.runner.hook.lock().unwrap() = Some(Arc::new(move |spec, _| {
        let handle = FakeHandle::new(&spec.task_id, None);
        started_handles.0.lock().unwrap().insert(spec.prompt.strip_prefix("do ").unwrap().into(), Arc::clone(&handle));
        started_handles.1.notify_all();
        Some(Ok(handle as Arc<dyn crate::manager::ManagedChildHandle>))
    }));
    let input = definition("failure-order", vec![node("graph-first-failure", &[]), node("wall-clock-first-failure", &[]), node("sibling-success", &[]), node("independent-root", &[]), node("skipped-dependent", &["graph-first-failure"]), node("sibling-dependent", &["sibling-success"]), node("independent-dependent", &["independent-root"])]);
    let started = fixture.start(input);
    let run_id = started.snapshot.run_id;
    let scheduler = create_dag_scheduler(DagSchedulerOptions { store: Arc::clone(&fixture.store), task_manager: Arc::clone(&fixture.tasks), initial_record: fixture.manager.record(&run_id, PARENT_SESSION_ID).unwrap(), execution_mode_agents: None, execution_mode_config: None, ancestry_depth: None, subscriber_ring: None, now: None }).unwrap();
    let (failure_tx, failure_rx) = mpsc::channel();
    let _unsubscribe = scheduler.subscribe(Arc::new(move |event| {
        if matches!(&event.payload, DagRunEventPayload::NodeTransitioned { node_id, to: DagNodeState::Failed, .. } if node_id == "wall-clock-first-failure") { failure_tx.send(()).unwrap(); }
    }));
    let run = std::thread::spawn(move || scheduler.run().unwrap());
    let wait_count = |count| {
        let state = handles.0.lock().unwrap();
        let (state, timeout) = handles.1.wait_timeout_while(state, Duration::from_secs(5), |state| state.len() < count).unwrap();
        assert!(!timeout.timed_out());
        state.clone()
    };
    let wave = wait_count(4);
    wave["wall-clock-first-failure"].fail(crate::runners::RunnerFailureKind::ChildPromptFailed, "failure:wall-clock-first-failure");
    recv_within(&failure_rx, "out of order failure");
    wave["sibling-success"].complete("output:sibling-success");
    wave["independent-root"].complete("output:independent-root");
    wave["graph-first-failure"].fail(crate::runners::RunnerFailureKind::ChildPromptFailed, "failure:graph-first-failure");
    let wave = wait_count(6);
    wave["sibling-dependent"].complete("output:sibling-dependent");
    wave["independent-dependent"].complete("output:independent-dependent");
    let result = run.join().unwrap();
    assert_eq!(result.nodes.iter().map(|node| node.state).collect::<Vec<_>>(), vec![DagNodeState::Failed, DagNodeState::Failed, DagNodeState::Completed, DagNodeState::Completed, DagNodeState::Skipped, DagNodeState::Completed, DagNodeState::Completed]);
    assert!(fixture.events(&run_id).iter().any(|event| matches!(&event.payload, DagRunEventPayload::RunFailed { error, .. } if error.node_id.as_deref() == Some("graph-first-failure") && error.message == "failure:graph-first-failure")));
}

struct PolicySession(std::sync::atomic::AtomicBool);

impl crate::runners::in_process::child_handle::ChildSession for PolicySession {
    fn session_id(&self) -> String { "policy-child".into() }
    fn prompt(&self, _: &str) -> Result<(), crate::host::HostError> { self.0.store(true, std::sync::atomic::Ordering::SeqCst); Ok(()) }
    fn steer(&self, _: &str) -> Result<(), crate::host::HostError> { Ok(()) }
    fn follow_up(&self, _: &str) -> Result<(), crate::host::HostError> { Ok(()) }
    fn abort(&self) -> Result<(), crate::host::HostError> { Ok(()) }
    fn subscribe(&self, _: crate::runners::in_process::child_handle::ChildSessionListener) -> crate::manager::Unsubscribe { Box::new(|| {}) }
    fn get_last_assistant_text(&self) -> Option<String> { self.0.load(std::sync::atomic::Ordering::SeqCst).then(|| "policy complete".into()) }
    fn dispose(&self) {}
}

struct PolicyTool(&'static str);

impl crate::runners::in_process::shared_tool_filter::ChildTool for PolicyTool {
    fn name(&self) -> &str { self.0 }
    fn description(&self) -> &str { self.0 }
    fn execute(&self, _: &str, _: &serde_json::Value) -> Result<serde_json::Value, crate::host::HostError> { Ok(serde_json::json!({"content":[]})) }
}

#[test]
fn real_inprocess_dag_child_omits_spawn_capable_parent_tools() {
    use crate::runners::in_process::{InProcessRunner, InProcessRunnerOptions};
    let mut fixture = scripted_fixture();
    let captured = Arc::new(std::sync::Mutex::new(None));
    let captured_options = Arc::clone(&captured);
    let runner = InProcessRunner::new(InProcessRunnerOptions {
        shared_parent_tools: ["read", "task", "task_send", "task_cancel", "team_create", "team_delete", "dag"].into_iter().map(|name| Arc::new(PolicyTool(name)) as crate::runners::in_process::shared_tool_filter::ChildToolRef).collect(), ui_only_tool_names: vec![], max_depth: None,
        create_session: Arc::new(move |options| { *captured_options.lock().unwrap() = Some(options); Ok(Arc::new(PolicySession(std::sync::atomic::AtomicBool::new(false)))) }),
    });
    let managed = crate::manager::runner::create_in_process_managed_runner(runner, crate::manager::runner::EmptySessionContext);
    let tasks = crate::manager::create_task_manager(crate::manager::types::TaskManagerOptions::new(fixture._project.store(), crate::manager::types::ManagedRunners { in_process: Arc::clone(&managed), process: managed }, crate::manager::manager_tests::fakes::category_planner(&[]), fixture._project.cwd()));
    fixture.tasks = Arc::new(tasks);
    let started = fixture.start(definition("child-policy", vec![node("policy", &[])]));
    assert_eq!(fixture.run(&started.snapshot.run_id).status, DagRunStatus::Completed);
    let options = captured.lock().unwrap().take().unwrap();
    assert_eq!(options.custom_tools.iter().map(|tool| tool.name()).collect::<Vec<_>>(), vec!["read"]);
    assert_eq!(options.resource_loader.extension_count(), 0);
}

#[test]
fn rpc_dag_child_drops_only_leading_inherited_extension() {
    let fixture = scripted_fixture();
    let inherited = vec!["/extensions/omo-senpi.js".to_string(), "/extensions/provider.js".to_string()];
    let started = fixture.start(definition("rpc-policy", vec![node("dag-rpc", &[])]));
    fixture.run(&started.snapshot.run_id);
    let dag_spec = fixture.runner.specs()[0].clone();
    let mut dag_rpc = crate::manager::runner::to_rpc_spec(&dag_spec);
    dag_rpc.extensions = Some(inherited.clone());
    let ordinary = crate::manager::manager_tests::fakes::started(fixture.tasks.start(&crate::manager::types::ManagerStartSpec { prompt: "ordinary rpc".into(), parent_session_id: PARENT_SESSION_ID.into(), root_session_id: Some(ROOT_SESSION_ID.into()), depth: 1, category: Some("quick".into()), execution_mode: Some(crate::manager::execution_mode::ExecutionMode::Process), extensions: Some(inherited.clone()), ..Default::default() }));
    fixture.tasks.wait_for(&ordinary.task_id, None, Some(Duration::from_secs(5))).unwrap();
    let ordinary_spec = fixture.runner.specs().last().unwrap().clone();
    let ordinary_rpc = crate::manager::runner::to_rpc_spec(&ordinary_spec);
    let extensions = |args: Vec<String>| args.windows(2).filter(|pair| pair[0] == "--extension").map(|pair| pair[1].clone()).collect::<Vec<_>>();
    assert_eq!(extensions(crate::runners::rpc::spawn::build_child_args(&dag_rpc)), vec![inherited[1].clone()]);
    assert_eq!(extensions(crate::runners::rpc::spawn::build_child_args(&ordinary_rpc)), inherited);
}

#[test]
fn real_manager_residency_cap_batches_a_wide_wave_without_failures() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use crate::manager::manager_tests::fakes::{FakeHandle, FakeRunner};
    let mut fixture = scripted_fixture();
    let residents = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let runner = FakeRunner::new();
    let handles = Arc::new((std::sync::Mutex::new(std::collections::BTreeMap::<String, Arc<FakeHandle>>::new()), std::sync::Condvar::new()));
    let started_handles = Arc::clone(&handles);
    *runner.hook.lock().unwrap() = Some(Arc::new(move |spec, _| {
        let handle = FakeHandle::new(&spec.task_id, None);
        started_handles.0.lock().unwrap().insert(spec.prompt.strip_prefix("do ").unwrap().into(), Arc::clone(&handle));
        started_handles.1.notify_all();
        Some(Ok(handle as Arc<dyn crate::manager::ManagedChildHandle>))
    }));
    let admission_residents = Arc::clone(&residents);
    let admission_peak = Arc::clone(&peak);
    let mut options = crate::manager::types::TaskManagerOptions::new(fixture._project.store(), crate::manager::types::ManagedRunners { in_process: Arc::clone(&runner) as Arc<dyn crate::manager::types::ManagedRunner>, process: Arc::clone(&runner) as Arc<dyn crate::manager::types::ManagedRunner> }, crate::manager::manager_tests::fakes::category_planner(&[]), fixture._project.cwd());
    options.config = config(16, 1);
    options.admit = Some(Arc::new(move |_| {
        if admission_residents.load(Ordering::SeqCst) >= 2 { return crate::manager::types::SpawnAdmission::Rejected { message: "resident cap reached".into() }; }
        let count = admission_residents.fetch_add(1, Ordering::SeqCst) + 1;
        admission_peak.fetch_max(count, Ordering::SeqCst);
        crate::manager::types::SpawnAdmission::Admitted
    }));
    fixture.tasks = Arc::new(crate::manager::create_task_manager(options));
    let input = definition("batched-wave", (0..7).map(|index| node(&format!("wide-{index}"), &[])).collect());
    let started = fixture.start(input);
    let run_id = started.snapshot.run_id;
    let scheduler = create_dag_scheduler(DagSchedulerOptions { store: Arc::clone(&fixture.store), task_manager: Arc::clone(&fixture.tasks), initial_record: fixture.manager.record(&run_id, PARENT_SESSION_ID).unwrap(), execution_mode_agents: None, execution_mode_config: None, ancestry_depth: None, subscriber_ring: None, now: None }).unwrap();
    let run = std::thread::spawn(move || scheduler.run().unwrap());
    for index in 0..7 {
        let id = format!("wide-{index}");
        let state = handles.0.lock().unwrap();
        let (state, timeout) = handles.1.wait_timeout_while(state, Duration::from_secs(5), |state| !state.contains_key(&id) || (index == 0 && state.len() < 2)).unwrap();
        assert!(!timeout.timed_out(), "{id} not admitted");
        let handle = Arc::clone(&state[&id]); drop(state);
        residents.fetch_sub(1, Ordering::SeqCst);
        handle.complete(&format!("output:{id}"));
    }
    let result = run.join().unwrap();
    assert_eq!(result.status, DagRunStatus::Completed);
    assert_eq!(peak.load(Ordering::SeqCst), 2);
    assert_eq!(runner.started_count(), 7);
    assert!(result.nodes.iter().all(|node| node.state == DagNodeState::Completed && node.error.is_none()));
    assert!(!fixture.events(&run_id).iter().any(|event| matches!(event.payload, DagRunEventPayload::NodeTransitioned { to: DagNodeState::Failed, .. })));
}
