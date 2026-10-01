//! `manager/manager-claim.test.ts`.
//!
//! The TS seed-floor cases fork a child process to get a fresh id floor; nextest
//! already runs every test in its own process, so they run inline here.

use std::sync::{Arc, Mutex};

use super::fakes::{
    FakeRunner, Harness, HarnessOptions, Project, TeamPortManager, base_spec, lock, make_manager,
    named, started,
};
use crate::manager::execution_mode::ExecutionMode;
use crate::manager::types::{ManagerStartSpec, StartResult, TaskManagerOptions};
use crate::state::{
    TaskRecord, TaskRecordInput, TaskStatus, bump_task_id, create_task_record, parse_task_id,
};
use crate::store::{StoreError, TaskRecordSaver, TaskRecordStore};
use crate::team::normalize::normalize_senpi_team_spec;
use crate::team::spawn_members::{SpawnMembersInput, spawn_team_members};
use serde_json::json;

type Saver = Arc<dyn TaskRecordSaver + Send + Sync>;

fn with_saver(project: Project, saver: Saver, now: Option<i64>) -> Harness {
    make_manager(HarnessOptions {
        project: Some(project),
        customize: Some(Box::new(move |options: &mut TaskManagerOptions| {
            options.record_saver = Some(saver);
            if let Some(now) = now {
                options.now = Some(Arc::new(move || now));
            }
        })),
        ..HarnessOptions::default()
    })
}

fn bump(id: &str) -> String {
    bump_task_id(parse_task_id(id).expect("task id"))
        .expect("bump")
        .to_string()
}

fn is_task_id(value: &str) -> bool {
    value.len() == 11
        && value.starts_with("st_")
        && value[3..]
            .chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
}

/// `collisionStore`: the first save loses to a foreign record claiming the same id.
struct CollisionSaver {
    inner: TaskRecordStore,
    first_candidate: Mutex<Option<String>>,
}

impl CollisionSaver {
    fn new(inner: TaskRecordStore) -> Arc<Self> {
        Arc::new(Self {
            inner,
            first_candidate: Mutex::new(None),
        })
    }

    fn first_candidate(&self) -> String {
        lock(&self.first_candidate)
            .clone()
            .expect("first candidate")
    }
}

impl TaskRecordSaver for CollisionSaver {
    fn save(&self, record: &TaskRecord) -> Result<(), StoreError> {
        let mut first = lock(&self.first_candidate);
        if first.is_none() {
            *first = Some(record.task_id.clone());
            self.inner.save(&TaskRecord {
                name: Some("foreign-winner".to_string()),
                ..record.clone()
            })?;
        }
        drop(first);
        self.inner.save(record)
    }
}

/// Rejects any save that would persist a duplicate sibling name.
struct UniqueNameSaver(TaskRecordStore);

impl TaskRecordSaver for UniqueNameSaver {
    fn save(&self, record: &TaskRecord) -> Result<(), StoreError> {
        let siblings = self.0.list()?.records;
        let duplicate = siblings.iter().any(|sibling| {
            sibling.parent_session_id == record.parent_session_id && sibling.name == record.name
        });
        assert!(
            !duplicate,
            "duplicate task name persisted: {:?}",
            record.name
        );
        self.0.save(record)
    }
}

/// `firstAllocationFailsStore`: the first save reports a collision at the top of the id space.
struct FirstAllocationFails {
    inner: TaskRecordStore,
    armed: Mutex<bool>,
}

impl TaskRecordSaver for FirstAllocationFails {
    fn save(&self, record: &TaskRecord) -> Result<(), StoreError> {
        if std::mem::replace(&mut *lock(&self.armed), false) {
            return Err(StoreError::Collision {
                task_id: parse_task_id("st_ffffffff").expect("task id"),
                path: "injected".into(),
            });
        }
        self.inner.save(record)
    }
}

fn seed_record(task_id: &str) -> TaskRecord {
    let draft = create_task_record(
        TaskRecordInput {
            parent_session_id: "parent-session".into(),
            root_session_id: "root-session".into(),
            depth: 1,
            execution_mode: "in-process".into(),
            model: "test/model".into(),
            ..TaskRecordInput::default()
        },
        None,
    )
    .expect("record");
    TaskRecord {
        task_id: task_id.to_string(),
        name: Some(task_id.to_string()),
        ..draft
    }
}

#[test]
fn given_no_requested_name_when_started_then_fallback_name_is_task_id() {
    let harness = make_manager(HarnessOptions::default());

    let task = started(harness.manager.start(&base_spec()));

    assert!(is_task_id(&task.name), "{}", task.name);
    assert_eq!(task.name, task.task_id);
}

#[test]
fn given_id_shaped_requested_name_when_unnamed_task_claims_fallback_then_sibling_names_unique() {
    let project = Project::new();
    let saver: Saver = Arc::new(UniqueNameSaver(project.store()));
    let bucket = chrono::Utc::now().timestamp_millis() / 65_536 + 10_000_000;
    let first_task_id = format!("st_{bucket:08x}");
    let harness = with_saver(project, saver, Some(bucket * 65_536));

    let first = started(harness.manager.start(&named(&bump(&first_task_id))));
    let second = started(harness.manager.start(&base_spec()));

    assert_eq!(first.task_id, first_task_id);
    assert_eq!(first.name, bump(&first_task_id));
    assert_eq!(second.task_id, bump(&first.name));
    assert_eq!(second.name, second.task_id);
}

#[test]
fn given_background_spec_when_started_then_manager_tracks_it_as_background() {
    let harness = make_manager(HarnessOptions::default());

    let task = started(harness.manager.start(&ManagerStartSpec {
        run_in_background: true,
        ..base_spec()
    }));

    assert!(harness.manager.was_background(&task.task_id));
}

#[test]
fn given_runner_failing_after_persistence_when_name_requested_again_then_record_terminal_and_name_reserved()
 {
    let runner = FakeRunner::new();
    runner.throw_on_start(true);
    let harness = make_manager(HarnessOptions {
        in_process: Some(Arc::clone(&runner)),
        ..HarnessOptions::default()
    });

    let failed = harness.manager.start(&named("durable-name"));
    runner.throw_on_start(false);
    let retried = started(harness.manager.start(&named("durable-name")));

    let StartResult::StartFailed(failed) = failed else {
        panic!("expected start_failed, got {failed:?}");
    };
    let record = harness.store.load(&failed.task_id).expect("load");
    assert_eq!(record.map(|record| record.status), Some(TaskStatus::Error));
    assert_eq!(retried.name, "durable-name-2");
    assert!(retried.name_warning.is_some());
}

#[test]
fn given_process_execution_mode_when_started_then_record_persists_spawn_spec() {
    let harness = make_manager(HarnessOptions::default());

    let task = started(harness.manager.start(&ManagerStartSpec {
        execution_mode: Some(ExecutionMode::Process),
        ..base_spec()
    }));

    let record = harness.store.load(&task.task_id).expect("load");
    assert!(record.and_then(|record| record.spawn_spec).is_some());
}

#[test]
fn given_blank_requested_name_when_started_then_falls_back_to_task_id() {
    for name in ["", "   "] {
        let harness = make_manager(HarnessOptions::default());

        let task = started(harness.manager.start(&named(name)));

        assert!(is_task_id(&task.name), "{name:?} -> {}", task.name);
        assert_eq!(task.name, task.task_id);
    }
}

#[test]
fn given_isolated_manager_process_when_seed_from_disk_then_next_id_follows_highest() {
    let project = Project::new();
    project
        .store()
        .save(&seed_record("st_7fffff00"))
        .expect("seed");
    let harness = make_manager(HarnessOptions {
        project: Some(project),
        ..HarnessOptions::default()
    });

    let task = started(harness.manager.start(&base_spec()));

    assert_eq!(task.task_id, "st_7fffff01");
}

#[test]
fn given_isolated_manager_process_when_warm_cache_seeds_from_disk_then_unlisted_file_counts() {
    let project = Project::new();
    let store = project.store();
    store.list().expect("warm list");
    let tasks_dir = store.state_dir().join("tasks");
    std::fs::create_dir_all(&tasks_dir).expect("tasks dir");
    std::fs::write(
        tasks_dir.join("st_7fffff10.json"),
        serde_json::to_string(&seed_record("st_7fffff10")).expect("json"),
    )
    .expect("write record");
    let harness = make_manager(HarnessOptions {
        project: Some(project),
        ..HarnessOptions::default()
    });

    let task = started(harness.manager.start(&base_spec()));

    assert_eq!(task.task_id, "st_7fffff11");
}

#[test]
fn given_isolated_manager_process_when_diagnostics_seed_from_disk_then_start_succeeds() {
    let project = Project::new();
    let store = project.store();
    store.save(&seed_record("st_00000020")).expect("seed");
    std::fs::write(
        store.state_dir().join("tasks").join("broken.json"),
        "{not-json",
    )
    .expect("write broken");
    let harness = make_manager(HarnessOptions {
        project: Some(project),
        ..HarnessOptions::default()
    });

    started(harness.manager.start(&base_spec()));
}

#[test]
fn given_foreign_winner_for_first_candidate_when_started_then_claims_next_id() {
    let project = Project::new();
    let inner = project.store();
    let collision = CollisionSaver::new(inner.clone());
    let harness = with_saver(project, Arc::clone(&collision) as Saver, None);

    let task = started(harness.manager.start(&base_spec()));

    let first = collision.first_candidate();
    assert_eq!(task.task_id, bump(&first));
    assert_eq!(task.name, task.task_id);
    let foreign = inner.load(&first).expect("load").expect("foreign");
    assert_eq!(foreign.name.as_deref(), Some("foreign-winner"));
    let own = inner.load(&task.task_id).expect("load").expect("own");
    assert_eq!(own.name.as_deref(), Some(task.task_id.as_str()));
}

#[test]
fn given_requested_name_and_foreign_winner_when_started_then_only_loser_keeps_requested_name() {
    let project = Project::new();
    let inner = project.store();
    let collision = CollisionSaver::new(inner.clone());
    let harness = with_saver(project, Arc::clone(&collision) as Saver, None);

    let task = started(harness.manager.start(&named("todo11")));

    let records: Vec<TaskRecord> = inner
        .list()
        .expect("list")
        .records
        .into_iter()
        .filter(|record| record.parent_session_id == "parent-1")
        .collect();
    let foreign = inner
        .load(&collision.first_candidate())
        .expect("load")
        .expect("foreign");
    assert_eq!(foreign.name.as_deref(), Some("foreign-winner"));
    assert_eq!(task.name, "todo11");
    let holders = records
        .iter()
        .filter(|record| record.name.as_deref() == Some("todo11"))
        .count();
    assert_eq!(holders, 1);
    let mut names: Vec<_> = records.iter().map(|record| record.name.clone()).collect();
    names.sort();
    names.dedup();
    assert_eq!(names.len(), records.len());
}

#[test]
fn given_duplicate_requested_name_in_one_parent_when_started_then_suffix_and_warning() {
    let harness = make_manager(HarnessOptions::default());
    started(harness.manager.start(&named("reviewer")));

    let second = started(harness.manager.start(&named("reviewer")));

    assert_eq!(second.name, "reviewer-2");
    assert!(second.name_warning.is_some());
}

#[test]
fn given_real_manager_under_collision_when_team_members_spawn_then_no_member_start_rejected() {
    let project = Project::new();
    let inner = project.store();
    let collision = CollisionSaver::new(inner);
    let harness = with_saver(project, collision as Saver, None);
    let spec = normalize_senpi_team_spec(
        &json!({
            "members": [{ "name": "alpha", "kind": "category", "category": "quick", "prompt": "work" }]
        }),
        "collision-team",
        None,
    )
    .expect("team spec normalizes");
    let manager = TeamPortManager::new(harness.manager.clone());
    let now = || 0i64;

    let result = spawn_team_members(&SpawnMembersInput {
        spec: &spec,
        team_run_id: "collision-run",
        manager: &manager,
        lead_session_id: "parent-1",
        spawn_depth: 1,
        max_parallel: 1,
        deadline_at: 1_000,
        now: &now,
        member_extension: None,
    });

    if let Some(failure) = &result.failure {
        assert!(!failure.message.contains("member_start_rejected"), "{}", failure.message);
    }
    assert!(result.failure.is_none(), "{:?}", result.failure);
    assert_eq!(result.spawned.len(), 1);
}

// `#given bookkeeping fails after a claim #when process mode starts #then it records a terminal
// failure` has no Rust counterpart to port: the TS case injects a store whose `replace` throws,
// while the Rust store port (`record_saver`) abstracts `save` alone and `replace`/`transition` stay
// concrete on `TaskRecordStore`. `manager.rs::bookkeeping_failed` ports the TS handler verbatim
// (same transitions, same assert, same `SPAWN_BOOKKEEPING_FAILED` message), but an injected store
// write failure cannot produce the TS outcome: the follow-up transitions would fail too, and the
// ported assert (`spawn bookkeeping failure transitions were not applied`) would fire first.

#[test]
fn given_allocation_failure_when_requested_name_retried_then_reservation_released() {
    let project = Project::new();
    let saver: Saver = Arc::new(FirstAllocationFails {
        inner: project.store(),
        armed: Mutex::new(true),
    });
    let harness = with_saver(project, saver, None);

    let failed = harness.manager.start(&named("todo11"));
    let retried = started(harness.manager.start(&named("todo11")));

    let StartResult::StartFailed(failed) = failed else {
        panic!("expected start_failed, got {failed:?}");
    };
    assert_eq!(failed.task_id, "");
    assert!(
        failed.error_message.contains("allocation failed"),
        "{}",
        failed.error_message
    );
    assert!(!failed.error_message.contains("already exists"));
    assert_eq!(retried.name, "todo11");
}

#[test]
fn given_whitespace_requested_name_when_collision_claimed_then_fallback_follows_final_id() {
    let project = Project::new();
    let collision = CollisionSaver::new(project.store());
    let harness = with_saver(project, collision as Saver, None);

    let task = started(harness.manager.start(&named("  ")));

    assert_eq!(task.name, task.task_id);
    assert_ne!(task.name, "task-1");
}
