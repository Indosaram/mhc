//! `dag/manager.test.ts`.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use pretty_assertions::assert_eq;

use super::*;
use crate::dag::graph::{DagDefinition, DagNodeInput};
use crate::dag::types::DagNodeTarget;
use crate::dag::store::{DagEventReadOptions, DagStoreConfig, DagStoreOptions, create_dag_file_store};
use crate::dag::types::DagNodeState;

const PARENT_SESSION_ID: &str = "ses_parent";
const OTHER_SESSION_ID: &str = "ses_other";
const ROOT_SESSION_ID: &str = "ses_root";

fn temp_project() -> std::path::PathBuf {
    tempfile::tempdir().expect("tempdir").keep()
}

fn plan_build_definition() -> DagDefinition {
    DagDefinition {
        key: "release-plan".to_string(),
        name: "release plan".to_string(),
        nodes: vec![
            DagNodeInput {
                id: "plan".to_string(),
                prompt: "draft the plan".to_string(),
                target: DagNodeTarget::Category("quick".to_string()),
                label: None,
                depends_on: None,
                task_summary: None,
                description: None,
                load_skills: None,
            },
            DagNodeInput {
                id: "build".to_string(),
                prompt: "build it".to_string(),
                target: DagNodeTarget::Category("quick".to_string()),
                label: None,
                depends_on: Some(vec!["plan".to_string()]),
                task_summary: None,
                description: None,
                load_skills: None,
            },
        ],
    }
}

fn counter_run_ids() -> Arc<dyn Fn() -> DagRunId + Send + Sync> {
    // One process-wide counter shared by every manager built in this test binary: the TS fixture
    // lets each manager mint real UUIDs, so two managers over the same store must never collide on
    // an id (a collision would let the second start overwrite the first run's record).
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    Arc::new(|| {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst) + 1;
        format!("run-{n}")
    })
}

fn open_store(project: &Path) -> Arc<DagFileStore> {
    Arc::new(
        create_dag_file_store(&DagStoreConfig::new(project), DagStoreOptions::default())
            .expect("store opens"),
    )
}

fn manager_over(store: &Arc<DagFileStore>) -> DagManager {
    create_dag_manager(DagManagerOptions {
        store: Arc::clone(store),
        new_run_id: Some(counter_run_ids()),
        now: None,
        materialize_skills: None,
        settings: None,
    })
}

fn run_files(store: &DagFileStore) -> Vec<String> {
    std::fs::read_dir(&store.paths.runs)
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|entry| entry.file_name().into_string().ok())
                .filter(|name| name.ends_with(".json"))
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn given_a_valid_two_node_definition_when_started_then_a_run_a_key_file_and_one_created_event_are_persisted()
 {
    let project = temp_project();
    let store = open_store(&project);
    let dag = manager_over(&store);

    let started = dag
        .start(DagStartParams {
            definition: plan_build_definition(),
            parent_session_id: PARENT_SESSION_ID.to_string(),
            root_session_id: ROOT_SESSION_ID.to_string(),
        })
        .expect("start");

    assert!(!started.reused);
    assert_eq!(started.snapshot.run_key, "release-plan");
    assert_eq!(started.snapshot.name, "release plan");
    assert_eq!(started.snapshot.parent_session_id, PARENT_SESSION_ID);
    assert_eq!(started.snapshot.root_session_id, ROOT_SESSION_ID);
    assert_eq!(started.snapshot.status, DagRunStatus::Pending);
    assert_eq!(started.snapshot.generation, 1);
    assert_eq!(
        started
            .snapshot
            .nodes
            .iter()
            .map(|node| format!("{}:{}", node.id, node_state_str(node.state)))
            .collect::<Vec<_>>(),
        vec!["plan:pending".to_string(), "build:pending".to_string()]
    );
    assert_eq!(
        started
            .snapshot
            .waves
            .iter()
            .map(|wave| wave.node_ids.join(","))
            .collect::<Vec<_>>(),
        vec!["plan".to_string(), "build".to_string()]
    );
    assert_eq!(
        started.snapshot.critical_path,
        vec!["plan".to_string(), "build".to_string()]
    );
    let key = store
        .read_key(PARENT_SESSION_ID, "release-plan")
        .expect("read key")
        .expect("key present");
    assert_eq!(key.run_id, started.snapshot.run_id);
    assert_eq!(
        key.definition_fingerprint,
        Some(started.snapshot.definition_fingerprint.clone())
    );
    let events = store
        .read_events(&started.snapshot.run_id, 0, &DagEventReadOptions { limit: 10, ..Default::default() })
        .expect("read events")
        .events;
    assert_eq!(events.len(), 1);
    let DagRunEventPayload::RunCreated {
        run_key,
        definition_fingerprint,
        node_count,
        edge_count,
        ..
    } = &events[0].payload
    else {
        panic!("expected dag.run.created");
    };
    assert_eq!(events[0].seq, 1);
    assert_eq!(run_key, "release-plan");
    assert_eq!(definition_fingerprint, &started.snapshot.definition_fingerprint);
    assert_eq!(*node_count, 2);
    assert_eq!(*edge_count, 1);
}

fn node_state_str(state: DagNodeState) -> &'static str {
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
fn given_a_run_created_from_a_definition_when_the_identical_definition_is_resubmitted_then_the_existing_run_is_reused_without_new_writes()
 {
    let project = temp_project();
    let store = open_store(&project);
    let dag = manager_over(&store);
    let first = dag
        .start(DagStartParams {
            definition: plan_build_definition(),
            parent_session_id: PARENT_SESSION_ID.to_string(),
            root_session_id: ROOT_SESSION_ID.to_string(),
        })
        .expect("first start");

    let second = dag
        .start(DagStartParams {
            definition: plan_build_definition(),
            parent_session_id: PARENT_SESSION_ID.to_string(),
            root_session_id: ROOT_SESSION_ID.to_string(),
        })
        .expect("second start");

    assert!(second.reused);
    assert_eq!(second.snapshot.run_id, first.snapshot.run_id);
    assert_eq!(run_files(&store).len(), 1);
    assert_eq!(
        store
            .read_events(&first.snapshot.run_id, 0, &DagEventReadOptions { limit: 10, ..Default::default() })
            .expect("read events")
            .events
            .len(),
        1
    );
}

#[test]
fn given_a_run_created_from_a_definition_when_the_same_key_is_resubmitted_with_an_edited_prompt_then_definition_conflict_is_returned_and_the_run_is_untouched()
 {
    let project = temp_project();
    let store = open_store(&project);
    let dag = manager_over(&store);
    let first = dag
        .start(DagStartParams {
            definition: plan_build_definition(),
            parent_session_id: PARENT_SESSION_ID.to_string(),
            root_session_id: ROOT_SESSION_ID.to_string(),
        })
        .expect("first start");
    let before = store
        .read_checkpoint::<DagRunRecordV1>(&first.snapshot.run_id)
        .expect("read checkpoint");

    let mut conflicting_definition = plan_build_definition();
    conflicting_definition.nodes = vec![DagNodeInput {
        id: "plan".to_string(),
        prompt: "draft the plan differently".to_string(),
        target: DagNodeTarget::Category("quick".to_string()),
        label: None,
        depends_on: None,
        task_summary: None,
        description: None,
        load_skills: None,
    }];
    let conflicting = dag.start(DagStartParams {
        definition: conflicting_definition,
        parent_session_id: PARENT_SESSION_ID.to_string(),
        root_session_id: ROOT_SESSION_ID.to_string(),
    });

    let error = conflicting.expect_err("expected definition_conflict");
    assert_eq!(error.code, DagManagerErrorCode::DefinitionConflict);
    assert_eq!(error.run_id, Some(first.snapshot.run_id.clone()));
    assert_eq!(
        store
            .read_checkpoint::<DagRunRecordV1>(&first.snapshot.run_id)
            .expect("read checkpoint"),
        before
    );
    assert_eq!(run_files(&store).len(), 1);
    assert_eq!(
        store
            .read_events(&first.snapshot.run_id, 0, &DagEventReadOptions { limit: 10, ..Default::default() })
            .expect("read events")
            .events
            .len(),
        1
    );
    assert_eq!(
        store
            .read_key(PARENT_SESSION_ID, "release-plan")
            .expect("read key")
            .and_then(|key| key.definition_fingerprint),
        Some(first.snapshot.definition_fingerprint.clone())
    );
}

#[test]
fn given_a_key_file_whose_run_record_was_pruned_when_a_changed_definition_reuses_the_key_then_a_fresh_run_is_created_instead_of_a_conflict()
 {
    let project = temp_project();
    let store = open_store(&project);
    let dag = manager_over(&store);
    let first = dag
        .start(DagStartParams {
            definition: plan_build_definition(),
            parent_session_id: PARENT_SESSION_ID.to_string(),
            root_session_id: ROOT_SESSION_ID.to_string(),
        })
        .expect("first start");
    std::fs::remove_file(store.paths.run(&first.snapshot.run_id)).expect("remove run file");

    let mut changed_definition = plan_build_definition();
    changed_definition.nodes[0].prompt = "a different plan".to_string();
    let second = dag
        .start(DagStartParams {
            definition: changed_definition,
            parent_session_id: PARENT_SESSION_ID.to_string(),
            root_session_id: ROOT_SESSION_ID.to_string(),
        })
        .expect("second start");

    assert!(!second.reused);
    assert_ne!(second.snapshot.run_id, first.snapshot.run_id);
    let key = store
        .read_key(PARENT_SESSION_ID, "release-plan")
        .expect("read key")
        .expect("key present");
    assert_eq!(key.run_id, second.snapshot.run_id);
    assert_eq!(
        key.definition_fingerprint,
        Some(second.snapshot.definition_fingerprint.clone())
    );
}

#[test]
fn given_a_definition_with_a_cyclic_dependency_when_started_then_invalid_definition_is_returned_and_nothing_is_persisted()
 {
    let project = temp_project();
    let store = open_store(&project);
    let dag = manager_over(&store);
    let mut cyclic = plan_build_definition();
    cyclic.nodes[0].depends_on = Some(vec!["build".to_string()]);
    cyclic.nodes[1].depends_on = Some(vec!["plan".to_string()]);

    let rejected = dag.start(DagStartParams {
        definition: cyclic,
        parent_session_id: PARENT_SESSION_ID.to_string(),
        root_session_id: ROOT_SESSION_ID.to_string(),
    });

    let error = rejected.expect_err("expected invalid_definition");
    assert_eq!(error.code, DagManagerErrorCode::InvalidDefinition);
    assert_eq!(
        error.errors.iter().map(|entry| entry.code).collect::<Vec<_>>(),
        vec![crate::dag::graph::DagCompileErrorCode::Cycle]
    );
    assert_eq!(run_files(&store), Vec::<String>::new());
    assert_eq!(
        std::fs::read_dir(&store.paths.keys).map(|e| e.count()).unwrap_or(0),
        0
    );
    assert_eq!(
        std::fs::read_dir(&store.paths.events).map(|e| e.count()).unwrap_or(0),
        0
    );
}

#[test]
fn given_a_configured_node_ceiling_when_a_definition_exceeds_it_then_invalid_definition_names_the_configured_bound()
 {
    let project = temp_project();
    let store = open_store(&project);
    let dag = create_dag_manager(DagManagerOptions {
        store: Arc::clone(&store),
        new_run_id: None,
        now: None,
        materialize_skills: None,
        settings: Some(DagSettings {
            max_nodes_per_run: 1,
            ..DAG_SETTINGS_DEFAULTS
        }),
    });

    let rejected = dag.start(DagStartParams {
        definition: plan_build_definition(),
        parent_session_id: PARENT_SESSION_ID.to_string(),
        root_session_id: ROOT_SESSION_ID.to_string(),
    });

    let error = rejected.expect_err("expected invalid_definition");
    assert_eq!(
        error.errors.iter().map(|entry| entry.code).collect::<Vec<_>>(),
        vec![crate::dag::graph::DagCompileErrorCode::NodeCountExceeded]
    );
    assert!(error.message.contains("max_nodes_per_run 1"));
    assert_eq!(run_files(&store), Vec::<String>::new());
}

#[test]
fn given_the_same_run_key_in_two_different_sessions_when_both_start_then_each_session_owns_its_own_run()
 {
    let project = temp_project();
    let store = open_store(&project);
    let dag = manager_over(&store);

    let mine = dag
        .start(DagStartParams {
            definition: plan_build_definition(),
            parent_session_id: PARENT_SESSION_ID.to_string(),
            root_session_id: ROOT_SESSION_ID.to_string(),
        })
        .expect("mine");
    let theirs = dag
        .start(DagStartParams {
            definition: plan_build_definition(),
            parent_session_id: OTHER_SESSION_ID.to_string(),
            root_session_id: ROOT_SESSION_ID.to_string(),
        })
        .expect("theirs");

    assert!(!theirs.reused);
    assert_ne!(theirs.snapshot.run_id, mine.snapshot.run_id);
    assert_eq!(run_files(&store).len(), 2);
}

#[test]
fn given_a_materialize_skills_hook_when_a_run_is_created_then_per_node_effective_prompt_slots_are_persisted_without_entering_the_fingerprint()
 {
    let project = temp_project();
    let store = open_store(&project);
    let plain = manager_over(&store);
    let with_skills = create_dag_manager(DagManagerOptions {
        store: Arc::clone(&store),
        new_run_id: Some(counter_run_ids()),
        now: None,
        materialize_skills: Some(Arc::new(|input: DagMaterializeSkillsInput<'_>| {
            DagSkillMaterialization {
                nodes: input
                    .definition
                    .nodes
                    .iter()
                    .map(|node| {
                        (
                            node.id.clone(),
                            format!("<skill name=\"x\">body</skill>\n\n{}", node.prompt),
                        )
                    })
                    .collect(),
                diagnostics: Vec::new(),
            }
        })),
        settings: None,
    });

    let materialized = with_skills
        .start(DagStartParams {
            definition: plan_build_definition(),
            parent_session_id: PARENT_SESSION_ID.to_string(),
            root_session_id: ROOT_SESSION_ID.to_string(),
        })
        .expect("materialized start");
    let bare = plain
        .start(DagStartParams {
            definition: plan_build_definition(),
            parent_session_id: OTHER_SESSION_ID.to_string(),
            root_session_id: ROOT_SESSION_ID.to_string(),
        })
        .expect("bare start");

    let materialized_record = with_skills
        .record(&materialized.snapshot.run_id, PARENT_SESSION_ID)
        .expect("materialized record");
    assert_eq!(
        materialized_record
            .definition
            .nodes
            .iter()
            .map(|node| node.effective_prompt.clone())
            .collect::<Vec<_>>(),
        vec![
            "<skill name=\"x\">body</skill>\n\ndraft the plan".to_string(),
            "<skill name=\"x\">body</skill>\n\nbuild it".to_string(),
        ]
    );
    let bare_record = plain
        .record(&bare.snapshot.run_id, OTHER_SESSION_ID)
        .expect("bare record");
    assert_eq!(
        bare_record
            .definition
            .nodes
            .iter()
            .map(|node| node.effective_prompt.clone())
            .collect::<Vec<_>>(),
        vec!["draft the plan".to_string(), "build it".to_string()]
    );
    assert_eq!(
        materialized.snapshot.definition_fingerprint,
        bare.snapshot.definition_fingerprint
    );
}

#[test]
fn given_a_run_owned_by_one_session_when_another_session_attaches_then_run_not_owned_is_returned()
 {
    let project = temp_project();
    let store = open_store(&project);
    let dag = manager_over(&store);
    let started = dag
        .start(DagStartParams {
            definition: plan_build_definition(),
            parent_session_id: PARENT_SESSION_ID.to_string(),
            root_session_id: ROOT_SESSION_ID.to_string(),
        })
        .expect("start");

    let error = dag
        .attach(&started.snapshot.run_id, OTHER_SESSION_ID)
        .expect_err("expected run_not_owned");
    assert_eq!(error.code, DagManagerErrorCode::RunNotOwned);
    assert_eq!(error.run_id, Some(started.snapshot.run_id.clone()));
    assert_eq!(
        dag.attach(&started.snapshot.run_id, PARENT_SESSION_ID)
            .expect("owner attaches")
            .run_id,
        started.snapshot.run_id
    );
}

#[test]
fn given_an_unknown_run_id_when_attached_or_snapshotted_then_run_not_found_is_returned() {
    let project = temp_project();
    let store = open_store(&project);
    let dag = manager_over(&store);
    let missing: DagRunId = "run-missing".to_string();

    assert_eq!(
        dag.attach(&missing, PARENT_SESSION_ID)
            .expect_err("expected run_not_found")
            .code,
        DagManagerErrorCode::RunNotFound
    );
    let error = dag
        .snapshot(&missing, PARENT_SESSION_ID)
        .expect_err("expected run_not_found");
    assert_eq!(error.code, DagManagerErrorCode::RunNotFound);
}

#[test]
fn given_a_run_owned_by_one_session_when_a_foreign_session_reads_history_then_run_not_owned_is_returned_and_no_events_leak()
 {
    let project = temp_project();
    let store = open_store(&project);
    let dag = manager_over(&store);
    let started = dag
        .start(DagStartParams {
            definition: plan_build_definition(),
            parent_session_id: PARENT_SESSION_ID.to_string(),
            root_session_id: ROOT_SESSION_ID.to_string(),
        })
        .expect("start");

    let foreign = dag.history(DagHistoryParams {
        run_id: started.snapshot.run_id.clone(),
        parent_session_id: OTHER_SESSION_ID.to_string(),
        since_seq: None,
        limit: None,
        lane: None,
        types: None,
        through_seq: None,
    });
    assert_eq!(
        foreign.expect_err("expected run_not_owned").code,
        DagManagerErrorCode::RunNotOwned
    );
    let page = dag
        .history(DagHistoryParams {
            run_id: started.snapshot.run_id.clone(),
            parent_session_id: PARENT_SESSION_ID.to_string(),
            since_seq: Some(0),
            limit: Some(10),
            lane: None,
            types: None,
            through_seq: None,
        })
        .expect("owner history");
    assert_eq!(
        page.events
            .iter()
            .map(|event| event.payload.event_type().as_str())
            .collect::<Vec<_>>(),
        vec!["dag.run.created"]
    );
    assert_eq!(page.next_since_seq, 1);
    assert_eq!(page.head_seq, 1);
    assert!(!page.has_more);
}

#[test]
fn given_journaled_events_when_history_pages_with_an_exclusive_since_and_a_through_bound_then_the_store_paging_contract_is_delegated()
 {
    let project = temp_project();
    let store = open_store(&project);
    let dag = manager_over(&store);
    let started = dag
        .start(DagStartParams {
            definition: plan_build_definition(),
            parent_session_id: PARENT_SESSION_ID.to_string(),
            root_session_id: ROOT_SESSION_ID.to_string(),
        })
        .expect("start");
    let run_id = started.snapshot.run_id.clone();
    store
        .append_event(&DagRunEvent {
            schema_version: SchemaVersion1,
            run_id: run_id.clone(),
            seq: 2,
            at: "2026-01-01T00:00:02.000Z".to_string(),
            lane: DagEventLane::Boundary,
            payload: DagRunEventPayload::RunStarted { generation: 1 },
        })
        .expect("append started");
    store
        .append_event(&DagRunEvent {
            schema_version: SchemaVersion1,
            run_id: run_id.clone(),
            seq: 3,
            at: "2026-01-01T00:00:03.000Z".to_string(),
            lane: DagEventLane::Boundary,
            payload: DagRunEventPayload::RunPaused {
                reason: Some("session_shutdown".to_string()),
            },
        })
        .expect("append paused");

    let page = dag
        .history(DagHistoryParams {
            run_id,
            parent_session_id: PARENT_SESSION_ID.to_string(),
            since_seq: Some(1),
            limit: Some(10),
            lane: None,
            types: None,
            through_seq: Some(2),
        })
        .expect("history");

    assert_eq!(
        page.events.iter().map(|event| event.seq).collect::<Vec<_>>(),
        vec![2]
    );
    assert_eq!(page.head_seq, 3);
    assert!(!page.has_more);
}

#[test]
fn given_runs_across_two_sessions_when_listed_then_only_this_sessions_runs_appear_sorted_by_updated_at_desc_then_run_id_asc()
 {
    let project = temp_project();
    let store = open_store(&project);
    let clock = Arc::new(std::sync::atomic::AtomicI64::new(
        chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00.000Z")
            .expect("parse")
            .timestamp_millis(),
    ));
    let clock_for_now = Arc::clone(&clock);
    let dag = create_dag_manager(DagManagerOptions {
        store: Arc::clone(&store),
        new_run_id: Some(counter_run_ids()),
        now: Some(Arc::new(move || clock_for_now.load(Ordering::SeqCst))),
        materialize_skills: None,
        settings: None,
    });
    let mut alpha = plan_build_definition();
    alpha.key = "alpha".to_string();
    let first = dag
        .start(DagStartParams {
            definition: alpha,
            parent_session_id: PARENT_SESSION_ID.to_string(),
            root_session_id: ROOT_SESSION_ID.to_string(),
        })
        .expect("alpha start");
    let mut beta = plan_build_definition();
    beta.key = "beta".to_string();
    let second = dag
        .start(DagStartParams {
            definition: beta,
            parent_session_id: PARENT_SESSION_ID.to_string(),
            root_session_id: ROOT_SESSION_ID.to_string(),
        })
        .expect("beta start");
    clock.fetch_add(60_000, Ordering::SeqCst);
    let mut gamma = plan_build_definition();
    gamma.key = "gamma".to_string();
    let third = dag
        .start(DagStartParams {
            definition: gamma,
            parent_session_id: PARENT_SESSION_ID.to_string(),
            root_session_id: ROOT_SESSION_ID.to_string(),
        })
        .expect("gamma start");
    let mut delta = plan_build_definition();
    delta.key = "delta".to_string();
    dag.start(DagStartParams {
        definition: delta,
        parent_session_id: OTHER_SESSION_ID.to_string(),
        root_session_id: ROOT_SESSION_ID.to_string(),
    })
    .expect("delta start");

    let listed = dag.list(PARENT_SESSION_ID, None).expect("list");

    assert_eq!(
        listed.iter().map(|entry| entry.run_id.clone()).collect::<Vec<_>>(),
        vec![
            third.snapshot.run_id.clone(),
            first.snapshot.run_id.clone(),
            second.snapshot.run_id.clone(),
        ]
    );
    assert_eq!(listed[0].run_key, "gamma");
    assert_eq!(listed[0].status, DagRunStatus::Pending);
    assert_eq!(listed[0].parent_session_id, PARENT_SESSION_ID);
}

#[test]
fn given_more_runs_than_the_requested_limit_when_listed_then_the_limit_is_clamped_to_at_most_256_and_defaults_to_100()
 {
    let project = temp_project();
    let store = open_store(&project);
    let dag = manager_over(&store);
    for index in 0..3 {
        let mut definition = plan_build_definition();
        definition.key = format!("key-{index}");
        dag.start(DagStartParams {
            definition,
            parent_session_id: PARENT_SESSION_ID.to_string(),
            root_session_id: ROOT_SESSION_ID.to_string(),
        })
        .expect("start");
    }

    let limited = dag.list(PARENT_SESSION_ID, Some(2)).expect("list limited");
    let clamped = dag.list(PARENT_SESSION_ID, Some(9_000)).expect("list clamped");

    assert_eq!(limited.len(), 2);
    assert_eq!(clamped.len(), 3);
    assert_eq!(
        dag.list(PARENT_SESSION_ID, Some(0))
            .expect_err("limit 0 rejected")
            .code,
        DagManagerErrorCode::InvalidArguments
    );
}

/// Rust equivalent of the TS cross-process race fixtures (`raceStarts` spawning two `Bun.spawn`
/// workers): both TS tests assert the SAME invariant that `with_key_lock` (identical file lock on
/// either side of the port) makes exactly one run win a same-key race. Real OS threads racing the
/// one shared file lock exercise that invariant without depending on a Bun-specific subprocess
/// harness.
#[test]
fn given_two_threads_starting_the_same_run_key_concurrently_when_both_race_then_exactly_one_run_file_exists_and_the_loser_reuses_it()
 {
    let project = temp_project();
    let store = open_store(&project);
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let handles: Vec<_> = (0..2)
        .map(|_| {
            let store = Arc::clone(&store);
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                let dag = create_dag_manager(DagManagerOptions {
                    store,
                    new_run_id: None,
                    now: None,
                    materialize_skills: None,
                    settings: None,
                });
                barrier.wait();
                dag.start(DagStartParams {
                    definition: plan_build_definition(),
                    parent_session_id: PARENT_SESSION_ID.to_string(),
                    root_session_id: ROOT_SESSION_ID.to_string(),
                })
            })
        })
        .collect();
    let outcomes: Vec<DagStartResult> = handles
        .into_iter()
        .map(|handle| handle.join().expect("thread join").expect("start succeeds"))
        .collect();

    assert_eq!(run_files(&store).len(), 1);
    let run_ids: std::collections::HashSet<_> = outcomes.iter().map(|o| o.snapshot.run_id.clone()).collect();
    assert_eq!(run_ids.len(), 1);
    assert_eq!(outcomes.iter().filter(|o| !o.reused).count(), 1);
    assert_eq!(outcomes.iter().filter(|o| o.reused).count(), 1);
}

/// Rust equivalent of the TS "racing the same key with different prompts" cross-process fixture:
/// same shared `with_key_lock` invariant, one winner creates the run and the conflicting
/// submission mutates nothing.
#[test]
fn given_two_threads_racing_the_same_key_with_different_prompts_when_both_race_then_one_run_is_created_and_the_conflicting_submission_mutates_nothing()
 {
    let project = temp_project();
    let store = open_store(&project);
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let prompts = ["draft the plan", "draft a different plan"];
    let handles: Vec<_> = prompts
        .iter()
        .map(|prompt| {
            let store = Arc::clone(&store);
            let barrier = Arc::clone(&barrier);
            let prompt = prompt.to_string();
            std::thread::spawn(move || {
                let dag = create_dag_manager(DagManagerOptions {
                    store,
                    new_run_id: None,
                    now: None,
                    materialize_skills: None,
                    settings: None,
                });
                let mut definition = plan_build_definition();
                definition.nodes[0].prompt = prompt;
                barrier.wait();
                dag.start(DagStartParams {
                    definition,
                    parent_session_id: PARENT_SESSION_ID.to_string(),
                    root_session_id: ROOT_SESSION_ID.to_string(),
                })
            })
        })
        .collect();
    let outcomes: Vec<Result<DagStartResult, DagManagerError>> = handles
        .into_iter()
        .map(|handle| handle.join().expect("thread join"))
        .collect();

    let runs = run_files(&store);
    assert_eq!(runs.len(), 1);
    let winners: Vec<&DagStartResult> = outcomes.iter().filter_map(|o| o.as_ref().ok()).collect();
    assert_eq!(winners.len(), 1);
    let conflicts: Vec<&DagManagerError> = outcomes
        .iter()
        .filter_map(|o| o.as_ref().err())
        .filter(|error| error.code == DagManagerErrorCode::DefinitionConflict)
        .collect();
    assert_eq!(conflicts.len(), 1);
    let winner_run_id = winners[0].snapshot.run_id.clone();
    let checkpoint = store
        .read_checkpoint::<DagRunRecordV1>(&winner_run_id)
        .expect("read checkpoint")
        .expect("checkpoint present");
    let key = store
        .read_key(PARENT_SESSION_ID, "release-plan")
        .expect("read key")
        .expect("key present");
    assert_eq!(key.run_id, winner_run_id);
    assert_eq!(
        key.definition_fingerprint,
        Some(checkpoint.definition_fingerprint.clone())
    );
    assert_eq!(
        store
            .read_events(&winner_run_id, 0, &DagEventReadOptions { limit: 10, ..Default::default() })
            .expect("read events")
            .events
            .iter()
            .map(|event| event.payload.event_type().as_str())
            .collect::<Vec<_>>(),
        vec!["dag.run.created"]
    );
}
