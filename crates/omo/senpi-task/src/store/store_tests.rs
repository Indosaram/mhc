use std::path::{Path, PathBuf};
use std::sync::{Arc, Barrier};

use pretty_assertions::assert_eq;
use serde_json::{Value, json};
use tempfile::TempDir;

use super::*;
use crate::state::{
    DeliverAs, PendingSteeringEntry, ResolvedModelRecord, ResolvedModelSource, SpawnSpecV1,
    TaskIdSpaceExhaustedError, TaskRecord, TaskRecordInput, TaskSpawnSpec, TaskStatus,
    TaskTransition, create_task_id, create_task_record, is_spawn_spec_v1, parse_task_id,
    transition_task_record,
};

fn project() -> TempDir {
    tempfile::Builder::new()
        .prefix("senpi-task-store-")
        .tempdir()
        .expect("tempdir")
}

fn config(project: &Path) -> StateDirConfig {
    StateDirConfig {
        project_dir: project.to_path_buf(),
        task_state_dir: None,
    }
}

fn store_for(project: &Path) -> TaskRecordStore {
    TaskRecordStore::new(&config(project))
}

fn input() -> TaskRecordInput {
    TaskRecordInput {
        parent_session_id: "parent-session".into(),
        root_session_id: "root-session".into(),
        depth: 0,
        execution_mode: "direct".into(),
        model: "gpt-5.2".into(),
        ..TaskRecordInput::default()
    }
}

fn base_record(task_id: &str) -> TaskRecord {
    TaskRecord {
        task_id: task_id.into(),
        ..create_task_record(input(), Some(0)).expect("id")
    }
}

fn claim_record(task_id: &str) -> TaskRecord {
    TaskRecord {
        name: Some(task_id.into()),
        execution_mode: "in-process".into(),
        model: "test/model".into(),
        created_at: "2026-07-20T00:00:00.000Z".into(),
        updated_at: "2026-07-20T00:00:00.000Z".into(),
        ..base_record(task_id)
    }
}

fn tasks_dir(project: &Path) -> PathBuf {
    resolve_state_dir(&config(project)).join("tasks")
}

fn write_persisted(project: &Path, task_id: &str, fields: Value) -> PathBuf {
    let mut record = json!({
        "task_id": task_id,
        "status": "pending",
        "residency_state": "resident",
        "parent_session_id": "parent-session",
        "root_session_id": "root-session",
        "depth": 0,
        "execution_mode": "direct",
        "model": "gpt-5.2",
        "created_at": "2026-07-06T00:00:00.000Z",
        "updated_at": "2026-07-06T00:00:00.000Z",
        "notification": { "run_epoch": 0, "notified_epoch": -1 },
    });
    for (key, value) in fields.as_object().expect("object") {
        record[key] = value.clone();
    }
    let dir = tasks_dir(project);
    std::fs::create_dir_all(&dir).expect("mkdir");
    let path = dir.join(format!("{task_id}.json"));
    std::fs::write(&path, record.to_string()).expect("write");
    path
}

fn parse_errors(result: &ListTaskRecordsResult) -> Vec<(PathBuf, String)> {
    result
        .diagnostics
        .iter()
        .filter_map(|diagnostic| match diagnostic {
            TaskRecordDiagnostic::ParseError { path, message } => {
                Some((path.clone(), message.clone()))
            }
            TaskRecordDiagnostic::ParseWarning { .. } => None,
        })
        .collect()
}

fn warnings(result: &ListTaskRecordsResult) -> Vec<PathBuf> {
    result
        .diagnostics
        .iter()
        .filter_map(|diagnostic| match diagnostic {
            TaskRecordDiagnostic::ParseWarning { path, .. } => Some(path.clone()),
            TaskRecordDiagnostic::ParseError { .. } => None,
        })
        .collect()
}

fn start(pid: i64) -> TaskTransition {
    TaskTransition::Start {
        timestamp: "2026-07-06T00:59:59.000Z".into(),
        pid: Some(pid),
        child_session_id: None,
    }
}

fn completed_record(record: TaskRecord) -> TaskRecord {
    let running = transition_task_record(&record, &start(9876)).record;
    transition_task_record(
        &running,
        &TaskTransition::Complete {
            timestamp: "2026-07-06T01:00:00.000Z".into(),
            final_response: "done".into(),
            run_stats: None,
        },
    )
    .record
}

// ---- state/store.test.ts ----

#[test]
fn state_dir_defaults_to_project_omo_senpi_task() {
    let dir = resolve_state_dir(&StateDirConfig {
        project_dir: "/tmp/project-a".into(),
        task_state_dir: None,
    });
    assert_eq!(dir, PathBuf::from("/tmp/project-a/.omo/senpi-task"));
}

#[test]
fn state_dir_override_wins() {
    let dir = resolve_state_dir(&StateDirConfig {
        project_dir: "/tmp/project-a".into(),
        task_state_dir: Some("/tmp/custom-state".into()),
    });
    assert_eq!(dir, PathBuf::from("/tmp/custom-state"));
}

#[test]
fn store_completed_record_round_trips() {
    let project = project();
    let store = store_for(project.path());
    let record = create_task_record(
        TaskRecordInput {
            name: Some("Summarize logs".into()),
            depth: 2,
            agent_type: Some("sisyphus".into()),
            execution_mode: "background".into(),
            tool_allow: Some(vec!["read".into(), "bash".into()]),
            tool_deny: Some(vec!["write".into()]),
            ..input()
        },
        None,
    )
    .expect("id");
    let completed = completed_record(record);
    store.save(&completed).expect("save");
    assert_eq!(
        store.load(&completed.task_id).expect("load"),
        Some(completed)
    );
}

#[test]
fn store_description_survives_round_trip() {
    let project = project();
    let store = store_for(project.path());
    let record = create_task_record(
        TaskRecordInput {
            name: Some("task-1".into()),
            description: Some("Audit the waiting-line renderers".into()),
            depth: 1,
            category: Some("quick".into()),
            execution_mode: "in-process".into(),
            ..input()
        },
        None,
    )
    .expect("id");
    store.save(&record).expect("save");
    assert_eq!(
        store
            .load(&record.task_id)
            .expect("load")
            .and_then(|record| record.description),
        Some("Audit the waiting-line renderers".into())
    );
}

#[test]
fn store_event_payload_is_redacted() {
    let project = project();
    let store = store_for(project.path());
    let record = create_task_record(input(), None).expect("id");
    store.save(&record).expect("save");
    let path = store
        .append_event(
            &record.task_id,
            &PersistedTaskEvent {
                event_type: "senpi_api".into(),
                payload: json!({ "apiKey": "redaction-sentinel", "nested": { "authorization": "Bearer redaction-sentinel" } }),
            },
        )
        .expect("append");
    let log = std::fs::read_to_string(path).expect("read");
    assert!(log.contains(r#""apiKey":"[REDACTED]""#));
    assert!(log.contains(r#""authorization":"[REDACTED]""#));
    assert!(!log.contains("redaction-sentinel"));
}

#[test]
fn store_corrupt_json_is_a_typed_diagnostic_and_skipped() {
    let project = project();
    let store = store_for(project.path());
    let good = create_task_record(input(), None).expect("id");
    store.save(&good).expect("save");
    let bad = tasks_dir(project.path()).join("st_badbeef.json");
    std::fs::write(&bad, "{not-json").expect("write");
    let result = store.list().expect("list");
    assert_eq!(
        result
            .records
            .iter()
            .map(|record| record.task_id.clone())
            .collect::<Vec<_>>(),
        vec![good.task_id]
    );
    let errors = parse_errors(&result);
    assert_eq!(errors.len(), result.diagnostics.len());
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0].0, bad);
    assert!(errors[0].1.contains("JSON"));
}

#[test]
fn store_duplicate_id_is_a_collision_and_keeps_the_original() {
    let project = project();
    let store = store_for(project.path());
    let original = create_task_record(input(), None).expect("id");
    let duplicate = TaskRecord {
        parent_session_id: "other-parent-session".into(),
        ..original.clone()
    };
    store.save(&original).expect("save");
    let collision = store.save(&duplicate);
    let Err(StoreError::Collision { task_id, .. }) = collision else {
        panic!("expected collision, got {collision:?}");
    };
    assert_eq!(task_id, parse_task_id(&original.task_id).expect("id"));
    assert_eq!(store.load(&original.task_id).expect("load"), Some(original));
}

#[test]
fn store_illegal_transition_is_rejected_and_completion_remains() {
    let project = project();
    let store = store_for(project.path());
    let completed = completed_record(create_task_record(input(), None).expect("id"));
    store.save(&completed).expect("save");
    let rejected = store
        .transition(&completed.task_id, &start(1234))
        .expect("transition");
    assert!(!rejected.applied);
    assert_eq!(
        store
            .load(&completed.task_id)
            .expect("load")
            .map(|record| record.status),
        Some(TaskStatus::Completed)
    );
}

// ---- store/record-store.test.ts ----

#[test]
fn record_store_agent_source_round_trips_without_diagnostics() {
    let project = project();
    let resolved =
        ResolvedModelRecord::new(ResolvedModelSource::Agent, "openai", "gpt-5.6-luna-fast");
    store_for(project.path())
        .save(&TaskRecord {
            resolved_model: Some(resolved.clone()),
            ..base_record("st_00000007")
        })
        .expect("save");
    let result = store_for(project.path()).list().expect("list");
    assert_eq!(result.diagnostics, Vec::new());
    assert_eq!(result.records[0].resolved_model, Some(resolved));
}

#[test]
fn record_store_liveness_epoch_round_trips() {
    let project = project();
    let mut record = base_record("st_00000008");
    record.notification.liveness_notified_epoch = Some(0);
    store_for(project.path()).save(&record).expect("save");
    let loaded = store_for(project.path())
        .load(&record.task_id)
        .expect("load");
    assert_eq!(
        loaded.and_then(|record| record.notification.liveness_notified_epoch),
        Some(0)
    );
}

#[test]
fn record_store_repeated_list_is_consistent() {
    let project = project();
    let store = store_for(project.path());
    for index in 1..=5 {
        store
            .save(&base_record(&format!("st_{index:08}")))
            .expect("save");
    }
    let ids = |result: ListTaskRecordsResult| {
        let mut ids: Vec<String> = result
            .records
            .into_iter()
            .map(|record| record.task_id)
            .collect();
        ids.sort();
        ids
    };
    let first = ids(store.list().expect("list"));
    assert_eq!(ids(store.list().expect("list")), first);
    assert_eq!(first.len(), 5);
}

#[test]
fn record_store_external_mutation_is_reflected() {
    let project = project();
    let store = store_for(project.path());
    store.save(&base_record("st_00000001")).expect("save");
    store.save(&base_record("st_00000002")).expect("save");
    store.list().expect("warm");
    let path_a = tasks_dir(project.path()).join("st_00000001.json");
    let mut raw: Value =
        serde_json::from_str(&std::fs::read_to_string(&path_a).expect("read")).expect("json");
    raw["name"] = json!("mutated-A");
    std::fs::write(&path_a, raw.to_string()).expect("write");
    let result = store.list().expect("list");
    let name_of = |id: &str| {
        result
            .records
            .iter()
            .find(|record| record.task_id == id)
            .and_then(|record| record.name.clone())
    };
    assert_eq!(name_of("st_00000001"), Some("mutated-A".into()));
    assert_ne!(name_of("st_00000002"), Some("mutated-A".into()));
}

#[test]
fn record_store_externally_removed_record_disappears() {
    let project = project();
    let store = store_for(project.path());
    store.save(&base_record("st_00000003")).expect("save");
    store.list().expect("warm");
    std::fs::remove_file(tasks_dir(project.path()).join("st_00000003.json")).expect("rm");
    assert_eq!(store.list().expect("list").records, Vec::new());
}

#[test]
fn record_store_serialized_patch_preserves_lifecycle_fields() {
    let project = project();
    let writer = store_for(project.path());
    let patcher = store_for(project.path());
    let record = base_record("st_00000009");
    writer.save(&record).expect("save");
    let mut advanced = TaskRecord {
        status: TaskStatus::Running,
        name: Some("revived-member".into()),
        ..record.clone()
    };
    advanced.notification.run_epoch = 1;
    writer.replace(&advanced).expect("replace");
    patcher
        .mutate(&record.task_id, |fresh| {
            if fresh.status != record.status
                || fresh.notification.run_epoch != record.notification.run_epoch
            {
                return fresh.clone();
            }
            let mut patched = fresh.clone();
            patched.notification.liveness_notified_epoch = Some(record.notification.run_epoch);
            patched
        })
        .expect("mutate");
    assert_eq!(writer.load(&record.task_id).expect("load"), Some(advanced));
}

#[test]
fn record_store_replace_is_visible_to_list() {
    let project = project();
    let store = store_for(project.path());
    let record = base_record("st_00000004");
    store.save(&record).expect("save");
    store.list().expect("warm");
    store
        .replace(&TaskRecord {
            name: Some("replaced".into()),
            ..record
        })
        .expect("replace");
    assert_eq!(
        store.list().expect("list").records[0].name,
        Some("replaced".into())
    );
}

#[test]
fn record_store_append_after_remove_recreates_the_log() {
    let project = project();
    let store = store_for(project.path());
    let event = |event_type: &str| PersistedTaskEvent {
        event_type: event_type.into(),
        payload: json!({}),
    };
    store.save(&base_record("st_00000005")).expect("save");
    store
        .append_event("st_00000005", &event("before"))
        .expect("append");
    store.remove("st_00000005").expect("remove");
    let path = store
        .append_event("st_00000005", &event("after"))
        .expect("append");
    let text = std::fs::read_to_string(path).expect("read");
    let lines: Vec<&str> = text.trim().lines().collect();
    assert_eq!(lines.len(), 1);
    let line: Value = serde_json::from_str(lines[0]).expect("json");
    assert_eq!(line["type"], json!("after"));
}

#[test]
fn record_store_writes_compact_json() {
    let project = project();
    store_for(project.path())
        .save(&base_record("st_00000006"))
        .expect("save");
    let raw =
        std::fs::read_to_string(tasks_dir(project.path()).join("st_00000006.json")).expect("read");
    assert!(!raw.contains("\n  "));
    let parsed: Value = serde_json::from_str(&raw).expect("json");
    assert_eq!(parsed["task_id"], json!("st_00000006"));
}

const ARTIFACT_TASK: &str = "st_00000010";

struct Artifacts {
    record: PathBuf,
    log: PathBuf,
    children: PathBuf,
    spill: PathBuf,
}

impl Artifacts {
    fn existing(&self) -> [bool; 4] {
        [&self.record, &self.log, &self.children, &self.spill].map(|path| path.exists())
    }
}

fn seed_artifacts(project: &Path, task_id: &str) -> Artifacts {
    let store = store_for(project);
    store.save(&base_record(task_id)).expect("save");
    store
        .append_event(
            task_id,
            &PersistedTaskEvent {
                event_type: "created".into(),
                payload: json!({}),
            },
        )
        .expect("append");
    let state = resolve_state_dir(&config(project));
    let child_dir = state
        .join("children")
        .join(task_id)
        .join("sessions")
        .join(task_id);
    std::fs::create_dir_all(&child_dir).expect("mkdir");
    std::fs::write(
        child_dir.join("x.jsonl"),
        "{\"role\":\"user\",\"content\":\"hi\"}\n",
    )
    .expect("write");
    std::fs::create_dir_all(state.join("completion-results")).expect("mkdir");
    std::fs::write(
        state
            .join("completion-results")
            .join(format!("{task_id}.txt")),
        "spilled final response",
    )
    .expect("write");
    Artifacts {
        record: state.join("tasks").join(format!("{task_id}.json")),
        log: state.join("logs").join(format!("{task_id}.jsonl")),
        children: state.join("children").join(task_id),
        spill: state
            .join("completion-results")
            .join(format!("{task_id}.txt")),
    }
}

#[test]
fn record_store_remove_deletes_all_four_artifacts() {
    let project = project();
    let artifacts = seed_artifacts(project.path(), ARTIFACT_TASK);
    assert_eq!(artifacts.existing(), [true; 4]);
    store_for(project.path())
        .remove(ARTIFACT_TASK)
        .expect("remove");
    assert_eq!(artifacts.existing(), [false; 4]);
}

#[test]
fn record_store_remove_handles_partial_cleanup() {
    let project = project();
    let artifacts = seed_artifacts(project.path(), ARTIFACT_TASK);
    std::fs::remove_dir_all(&artifacts.children).expect("rm");
    assert_eq!(artifacts.existing(), [true, true, false, true]);
    store_for(project.path())
        .remove(ARTIFACT_TASK)
        .expect("remove");
    assert_eq!(artifacts.existing(), [false; 4]);
}

#[test]
fn record_store_remove_twice_is_a_no_op() {
    let project = project();
    let artifacts = seed_artifacts(project.path(), ARTIFACT_TASK);
    let store = store_for(project.path());
    store.remove(ARTIFACT_TASK).expect("remove");
    assert_eq!(artifacts.existing(), [false; 4]);
    store.remove(ARTIFACT_TASK).expect("second remove");
    assert_eq!(artifacts.existing(), [false; 4]);
}

const TOMBSTONE_TASK: &str = "st_00000030";

#[test]
fn record_store_tombstone_hides_record_from_load_list_and_mutate() {
    let project = project();
    let artifacts = seed_artifacts(project.path(), TOMBSTONE_TASK);
    let store = store_for(project.path());
    let tombstone = tasks_dir(project.path()).join(format!("{TOMBSTONE_TASK}.json.expunging"));
    std::fs::write(
        tasks_dir(project.path()).join("not-a-task.json.expunging"),
        "{}",
    )
    .expect("write");
    let result = store
        .tombstone_if_expired(TOMBSTONE_TASK, |_| false)
        .expect("tombstone");
    let TombstoneResult::Tombstoned(record) = result else {
        panic!("expected tombstoned, got {result:?}");
    };
    assert_eq!(record.task_id, TOMBSTONE_TASK);
    assert!(!artifacts.record.exists());
    assert!(tombstone.exists());
    assert_eq!(store.load(TOMBSTONE_TASK).expect("load"), None);
    assert_eq!(store.list().expect("list").records, Vec::new());
    assert_eq!(
        store.mutate(TOMBSTONE_TASK, Clone::clone).expect("mutate"),
        None
    );
    assert_eq!(
        store.list_expunging().expect("list"),
        vec![TOMBSTONE_TASK.to_string()]
    );
}

#[test]
fn record_store_retained_record_is_untouched() {
    let project = project();
    let artifacts = seed_artifacts(project.path(), TOMBSTONE_TASK);
    let store = store_for(project.path());
    let result = store
        .tombstone_if_expired(TOMBSTONE_TASK, |_| true)
        .expect("tombstone");
    assert_eq!(result, TombstoneResult::Retained);
    assert!(artifacts.record.exists());
    assert!(
        !tasks_dir(project.path())
            .join(format!("{TOMBSTONE_TASK}.json.expunging"))
            .exists()
    );
    assert_eq!(
        store
            .load(TOMBSTONE_TASK)
            .expect("load")
            .map(|record| record.task_id),
        Some(TOMBSTONE_TASK.to_string())
    );
}

#[test]
fn record_store_missing_record_reports_missing() {
    let project = project();
    let store = store_for(project.path());
    let result = store
        .tombstone_if_expired(TOMBSTONE_TASK, |_| false)
        .expect("tombstone");
    assert_eq!(result, TombstoneResult::Missing);
    assert_eq!(store.list_expunging().expect("list"), Vec::<String>::new());
}

#[test]
fn record_store_complete_expunge_removes_everything_idempotently() {
    let project = project();
    let artifacts = seed_artifacts(project.path(), TOMBSTONE_TASK);
    let store = store_for(project.path());
    store
        .tombstone_if_expired(TOMBSTONE_TASK, |_| false)
        .expect("tombstone");
    let tombstone = tasks_dir(project.path()).join(format!("{TOMBSTONE_TASK}.json.expunging"));
    assert!(tombstone.exists());
    store.complete_expunge(TOMBSTONE_TASK).expect("expunge");
    assert!(!tombstone.exists());
    assert_eq!(artifacts.existing(), [false; 4]);
    assert_eq!(store.list_expunging().expect("list"), Vec::<String>::new());
    store
        .complete_expunge(TOMBSTONE_TASK)
        .expect("second expunge");
    assert!(!tombstone.exists());
}

// ---- store/claim.test.ts ----

#[test]
fn claim_colliding_draft_saves_next_available_id() {
    let project = project();
    let store = store_for(project.path());
    store.save(&claim_record("st_00000010")).expect("save");
    store.save(&claim_record("st_00000011")).expect("save");
    let claimed = claim_task_record(
        &store,
        &claim_record("st_00000010"),
        &ClaimOptions::default(),
    )
    .expect("claim");
    assert_eq!(claimed.task_id, "st_00000012");
    assert!(tasks_dir(project.path()).join("st_00000012.json").exists());
}

#[test]
fn claim_final_collision_propagates_past_attempt_limit() {
    let project = project();
    let store = store_for(project.path());
    for id in ["st_00000010", "st_00000011", "st_00000012"] {
        store.save(&claim_record(id)).expect("save");
    }
    let result = claim_task_record(
        &store,
        &claim_record("st_00000010"),
        &ClaimOptions {
            max_attempts: 2,
            ..ClaimOptions::default()
        },
    );
    assert!(matches!(
        result,
        Err(ClaimError::Store(StoreError::Collision { .. }))
    ));
}

#[test]
fn claim_name_follows_bumped_id() {
    let project = project();
    let store = store_for(project.path());
    store.save(&claim_record("st_00000010")).expect("save");
    let claimed = claim_task_record(
        &store,
        &claim_record("st_00000010"),
        &ClaimOptions {
            name_binding: NameBinding::FollowsId,
            ..ClaimOptions::default()
        },
    )
    .expect("claim");
    assert_eq!(claimed.name, Some(claimed.task_id.clone()));
}

#[test]
fn claim_skips_unavailable_id_derived_name() {
    let project = project();
    let store = store_for(project.path());
    let name_available = |name: &str| name != "st_00000010";
    let claimed = claim_task_record(
        &store,
        &claim_record("st_00000010"),
        &ClaimOptions {
            name_binding: NameBinding::FollowsId,
            name_available: Some(&name_available),
            ..ClaimOptions::default()
        },
    )
    .expect("claim");
    assert_eq!(
        (claimed.task_id.as_str(), claimed.name.as_deref()),
        ("st_00000011", Some("st_00000011"))
    );
}

#[test]
fn claim_unavailable_names_exhaust_the_budget() {
    let project = project();
    let store = store_for(project.path());
    let never = |_: &str| false;
    let result = claim_task_record(
        &store,
        &claim_record("st_00000010"),
        &ClaimOptions {
            max_attempts: 2,
            name_binding: NameBinding::FollowsId,
            name_available: Some(&never),
        },
    );
    assert!(matches!(
        result,
        Err(ClaimError::Exhausted(TaskIdSpaceExhaustedError))
    ));
}

#[test]
fn claim_preserves_requested_name_without_follow() {
    let project = project();
    let store = store_for(project.path());
    store.save(&claim_record("st_00000010")).expect("save");
    let claimed = claim_task_record(
        &store,
        &claim_record("st_00000010"),
        &ClaimOptions::default(),
    )
    .expect("claim");
    assert_eq!(
        (claimed.task_id.as_str(), claimed.name.as_deref()),
        ("st_00000011", Some("st_00000010"))
    );
}

struct FailingSaver;

impl TaskRecordSaver for FailingSaver {
    fn save(&self, _record: &TaskRecord) -> Result<(), StoreError> {
        Err(StoreError::Io(std::io::Error::other("store unavailable")))
    }
}

#[test]
fn claim_non_collision_error_propagates_unwrapped() {
    let result = claim_task_record(
        &FailingSaver,
        &claim_record("st_00000010"),
        &ClaimOptions::default(),
    );
    let Err(ClaimError::Store(StoreError::Io(error))) = result else {
        panic!("expected io error, got {result:?}");
    };
    assert_eq!(error.to_string(), "store unavailable");
}

#[test]
fn claim_floor_follows_the_claim() {
    let project = project();
    let store = store_for(project.path());
    store.save(&claim_record("st_00000030")).expect("save");
    let claimed = claim_task_record(
        &store,
        &claim_record("st_00000030"),
        &ClaimOptions {
            name_binding: NameBinding::FollowsId,
            ..ClaimOptions::default()
        },
    )
    .expect("claim");
    assert_eq!(claimed.task_id, "st_00000031");
    assert_eq!(
        create_task_id(Some(0x10 * 0x10000))
            .expect("id")
            .to_string(),
        "st_00000032"
    );
}

// ---- store/claim-race.test.ts ----
// The TS race runs two OS processes; here two threads with independent stores contend on the same
// directory from one frozen id bucket, released together by a barrier (no timing assumptions).
#[test]
fn claim_race_two_claimers_one_bucket_all_records_unique() {
    let project = project();
    let frozen_now_ms = 0x10 * 0x10000;
    let barrier = Arc::new(Barrier::new(2));
    let handles: Vec<_> = ["a", "b"]
        .into_iter()
        .map(|tag| {
            let project_dir = project.path().to_path_buf();
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || {
                let store = store_for(&project_dir);
                let mut factory = crate::state::create_task_id_factory(move || frozen_now_ms);
                barrier.wait();
                let mut ids = Vec::new();
                let mut retries = 0;
                for _ in 0..50 {
                    let id = factory.next_id().expect("id").to_string();
                    let draft = TaskRecord {
                        parent_session_id: format!("race-{tag}"),
                        root_session_id: format!("race-{tag}"),
                        depth: 1,
                        ..claim_record(&id)
                    };
                    let claimed = claim_task_record(
                        &store,
                        &draft,
                        &ClaimOptions {
                            name_binding: NameBinding::FollowsId,
                            ..ClaimOptions::default()
                        },
                    )
                    .expect("claim");
                    if claimed.task_id != draft.task_id {
                        retries += 1;
                    }
                    ids.push(claimed.task_id);
                }
                (ids, retries)
            })
        })
        .collect();
    let results: Vec<(Vec<String>, usize)> = handles
        .into_iter()
        .map(|handle| handle.join().expect("join"))
        .collect();
    let mut ids: Vec<String> = results.iter().flat_map(|(ids, _)| ids.clone()).collect();
    assert_eq!(ids.len(), 100);
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), 100);
    assert_eq!(
        std::fs::read_dir(tasks_dir(project.path()))
            .expect("dir")
            .count(),
        100
    );
    assert!(results.iter().map(|(_, retries)| retries).sum::<usize>() >= 1);
}

// ---- store/runtime-fallback-record.test.ts ----

#[test]
fn runtime_fallback_chain_round_trips() {
    let project = project();
    let requested = ResolvedModelRecord {
        reasoning_effort: Some("minimal".into()),
        ..ResolvedModelRecord::new(
            ResolvedModelSource::Category,
            "kimi-coding",
            "kimi-for-coding-highspeed-unlocked",
        )
    };
    let fallback = vec![ResolvedModelRecord {
        reasoning_effort: Some("minimal".into()),
        ..ResolvedModelRecord::new(
            ResolvedModelSource::Category,
            "quotio-openai",
            "gpt-5.6-luna-fast",
        )
    }];
    let record = create_task_record(
        TaskRecordInput {
            parent_session_id: "parent-1".into(),
            root_session_id: "parent-1".into(),
            depth: 1,
            execution_mode: "in-process".into(),
            model: requested.display.clone(),
            requested_model: Some(requested.clone()),
            fallback_models: Some(fallback.clone()),
            resolved_model: Some(requested.clone()),
            ..TaskRecordInput::default()
        },
        None,
    )
    .expect("id");
    store_for(project.path()).save(&record).expect("save");
    let loaded = store_for(project.path()).list().expect("list");
    assert_eq!(loaded.diagnostics, Vec::new());
    assert_eq!(loaded.records[0].requested_model, Some(requested));
    assert_eq!(loaded.records[0].fallback_models, Some(fallback));
}

// ---- store/record-parse-v1.test.ts ----

fn list(project: &Path) -> ListTaskRecordsResult {
    store_for(project).list().expect("list")
}

#[test]
fn parse_v1_legacy_record_defaults_notify_false() {
    let project = project();
    write_persisted(project.path(), "st_02000001", json!({}));
    let result = list(project.path());
    assert_eq!(result.diagnostics, Vec::new());
    assert!(!result.records[0].notify_on_terminal);
}

#[test]
fn parse_v1_notify_true_round_trips() {
    let project = project();
    write_persisted(
        project.path(),
        "st_02000002",
        json!({ "notify_on_terminal": true }),
    );
    let result = list(project.path());
    assert_eq!(result.diagnostics, Vec::new());
    assert!(result.records[0].notify_on_terminal);
}

#[test]
fn parse_v1_legacy_spawn_spec_drops_untrusted_inputs() {
    let project = project();
    write_persisted(
        project.path(),
        "st_02000003",
        json!({ "spawn_spec": { "cwd": "/safe/project", "extensions": ["/tmp/malicious.ts"], "member_env": { "MALICIOUS": "execute-me" } } }),
    );
    let result = list(project.path());
    assert_eq!(result.diagnostics, Vec::new());
    let spec = result.records[0].spawn_spec.clone().expect("spec");
    assert!(!is_spawn_spec_v1(&spec));
    assert_eq!(
        serde_json::to_value(&spec).expect("json"),
        json!({ "cwd": "/safe/project" })
    );
}

#[test]
fn parse_v1_full_spec_round_trips() {
    let project = project();
    let spec = json!({
        "version": 1,
        "cwd": "/project/child",
        "prompt": "Fix the flaky test in auth.ts",
        "instructions": "Use bun test, not jest",
        "member_scoped_tool_names": ["read", "bash"],
    });
    write_persisted(project.path(), "st_02000004", json!({ "spawn_spec": spec }));
    let result = list(project.path());
    assert_eq!(result.diagnostics, Vec::new());
    let parsed = result.records[0].spawn_spec.clone().expect("spec");
    assert!(is_spawn_spec_v1(&parsed));
    assert_eq!(serde_json::to_value(&parsed).expect("json"), spec);
}

#[test]
fn parse_v1_minimal_spec_round_trips() {
    let project = project();
    write_persisted(
        project.path(),
        "st_02000005",
        json!({ "spawn_spec": { "version": 1, "cwd": "/project/child", "prompt": "Do the thing" } }),
    );
    let result = list(project.path());
    assert_eq!(result.diagnostics, Vec::new());
    assert_eq!(
        result.records[0].spawn_spec,
        Some(TaskSpawnSpec::V1(SpawnSpecV1 {
            cwd: "/project/child".into(),
            prompt: "Do the thing".into(),
            instructions: None,
            member_scoped_tool_names: None,
        }))
    );
}

fn assert_single_parse_error(result: &ListTaskRecordsResult) {
    assert_eq!(result.records, Vec::new());
    assert_eq!(result.diagnostics.len(), 1);
    assert_eq!(parse_errors(result).len(), 1);
}

#[test]
fn parse_v1_spec_missing_cwd_is_parse_error() {
    let project = project();
    write_persisted(
        project.path(),
        "st_02000006",
        json!({ "spawn_spec": { "version": 1, "prompt": "no cwd" } }),
    );
    assert_single_parse_error(&list(project.path()));
}

#[test]
fn parse_v1_spec_missing_prompt_is_parse_error() {
    let project = project();
    write_persisted(
        project.path(),
        "st_02000007",
        json!({ "spawn_spec": { "version": 1, "cwd": "/project/child" } }),
    );
    assert_single_parse_error(&list(project.path()));
}

fn steering(id: &str, message: &str, deliver_as: DeliverAs) -> PendingSteeringEntry {
    PendingSteeringEntry {
        id: id.into(),
        message: message.into(),
        deliver_as,
    }
}

#[test]
fn parse_v1_valid_steering_round_trips() {
    let project = project();
    write_persisted(
        project.path(),
        "st_02000010",
        json!({ "pending_steering": [
            { "id": "msg-1", "message": "focus on the auth module", "deliver_as": "steer" },
            { "id": "msg-2", "message": "also check the db layer", "deliver_as": "followUp" },
        ] }),
    );
    let result = list(project.path());
    assert_eq!(result.diagnostics, Vec::new());
    assert_eq!(
        result.records[0].pending_steering,
        Some(vec![
            steering("msg-1", "focus on the auth module", DeliverAs::Steer),
            steering("msg-2", "also check the db layer", DeliverAs::FollowUp),
        ])
    );
}

#[test]
fn parse_v1_malformed_steering_entries_are_dropped_with_warning() {
    let project = project();
    write_persisted(
        project.path(),
        "st_02000011",
        json!({ "pending_steering": [
            { "id": "good-1", "message": "survive", "deliver_as": "steer" },
            { "message": "no id", "deliver_as": "steer" },
            { "id": "bad-deliver", "message": "wrong type", "deliver_as": "invalid" },
            "not-an-object",
            { "id": "no-msg", "deliver_as": "followUp" },
            { "id": "good-2", "message": "also survives", "deliver_as": "followUp" },
        ] }),
    );
    let result = list(project.path());
    assert_eq!(result.records.len(), 1);
    assert_eq!(result.records[0].task_id, "st_02000011");
    assert_eq!(
        result.records[0].pending_steering,
        Some(vec![
            steering("good-1", "survive", DeliverAs::Steer),
            steering("good-2", "also survives", DeliverAs::FollowUp),
        ])
    );
    let warned = warnings(&result);
    assert!(!warned.is_empty());
    assert!(warned[0].to_string_lossy().contains("st_02000011"));
}

#[test]
fn parse_v1_non_array_steering_is_parse_error() {
    let project = project();
    write_persisted(
        project.path(),
        "st_02000012",
        json!({ "pending_steering": "not-an-array" }),
    );
    assert_single_parse_error(&list(project.path()));
}

#[test]
fn parse_v1_object_steering_is_parse_error() {
    let project = project();
    write_persisted(
        project.path(),
        "st_02000013",
        json!({ "pending_steering": { "id": "wrong", "message": "shape", "deliver_as": "steer" } }),
    );
    assert_single_parse_error(&list(project.path()));
}

#[test]
fn parse_v1_all_malformed_steering_keeps_record_with_warnings() {
    let project = project();
    write_persisted(
        project.path(),
        "st_02000014",
        json!({ "pending_steering": [{ "id": "bad", "message": "ok", "deliver_as": "invalid-type" }] }),
    );
    let result = list(project.path());
    assert_eq!(result.records.len(), 1);
    assert_eq!(result.records[0].task_id, "st_02000014");
    assert_eq!(result.records[0].pending_steering, None);
    assert!(!warnings(&result).is_empty());
}

#[test]
fn parse_v1_warning_only_names_the_bad_sibling() {
    let project = project();
    write_persisted(
        project.path(),
        "st_02000020",
        json!({ "pending_steering": [{ "message": "no id", "deliver_as": "steer" }] }),
    );
    write_persisted(
        project.path(),
        "st_02000021",
        json!({ "pending_steering": [{ "id": "ok", "message": "good", "deliver_as": "steer" }] }),
    );
    let result = list(project.path());
    let mut ids: Vec<String> = result
        .records
        .iter()
        .map(|record| record.task_id.clone())
        .collect();
    ids.sort();
    assert_eq!(
        ids,
        vec!["st_02000020".to_string(), "st_02000021".to_string()]
    );
    let warned = warnings(&result);
    assert!(
        warned
            .iter()
            .all(|path| path.to_string_lossy().contains("st_02000020"))
    );
    assert!(
        !warned
            .iter()
            .any(|path| path.to_string_lossy().contains("st_02000021"))
    );
}

// ---- store/security.test.ts ----

#[test]
fn security_hostile_task_ids_are_rejected_before_writing() {
    let project = project();
    let store = store_for(project.path());
    let outside = project
        .path()
        .join("outside-write")
        .to_string_lossy()
        .to_string();
    let hostile = [
        "../outside-write",
        "nested/../../outside-write",
        outside.as_str(),
        "file://outside-write",
        "st_12345678%2foutside-write",
        "st_12345678\\outside-write",
    ];
    for task_id in hostile {
        let invalid = |result: Result<(), StoreError>| matches!(result, Err(StoreError::InvalidTaskId(ref error)) if error.to_string().contains("Invalid task id"));
        assert!(invalid(store.save(&base_record(task_id))), "{task_id}");
        assert!(invalid(store.load(task_id).map(|_| ())), "{task_id}");
        assert!(
            invalid(
                store
                    .append_event(
                        task_id,
                        &PersistedTaskEvent {
                            event_type: "probe".into(),
                            payload: json!({}),
                        }
                    )
                    .map(|_| ())
            ),
            "{task_id}"
        );
        assert!(
            invalid(store.transition(task_id, &start(1234)).map(|_| ())),
            "{task_id}"
        );
    }
    assert!(!project.path().join("outside-write.jsonl").exists());
}

fn resolved_model_fixture() -> Value {
    json!({ "provider": "openai", "model_id": "gpt-5.6-sol", "display": "OpenAI GPT-5.6 SOL", "source": "category" })
}

fn with_fields(base: Value, extra: Value) -> Value {
    let mut merged = base;
    for (key, value) in extra.as_object().expect("object") {
        merged[key] = value.clone();
    }
    merged
}

#[test]
fn security_old_record_without_resolved_model_is_valid() {
    let project = project();
    write_persisted(project.path(), "st_01d00001", json!({}));
    let result = list(project.path());
    assert_eq!(result.diagnostics, Vec::new());
    assert_eq!(result.records[0].resolved_model, None);
}

#[test]
fn security_resolved_model_metadata_round_trips() {
    let project = project();
    let resolved = with_fields(
        resolved_model_fixture(),
        json!({ "variant": "high", "reasoning_effort": "medium" }),
    );
    write_persisted(
        project.path(),
        "st_01d00002",
        json!({ "resolved_model": resolved }),
    );
    let result = list(project.path());
    assert_eq!(result.diagnostics, Vec::new());
    assert_eq!(
        serde_json::to_value(&result.records[0].resolved_model).expect("json"),
        resolved
    );
}

#[test]
fn security_reasoning_effort_only_round_trips() {
    let project = project();
    let resolved = with_fields(
        resolved_model_fixture(),
        json!({ "reasoning_effort": "high" }),
    );
    write_persisted(
        project.path(),
        "st_1d00002b",
        json!({ "resolved_model": resolved }),
    );
    let result = list(project.path());
    assert_eq!(result.diagnostics, Vec::new());
    assert_eq!(
        serde_json::to_value(&result.records[0].resolved_model).expect("json"),
        resolved
    );
}

#[test]
fn security_future_resolved_model_field_is_ignored() {
    let project = project();
    write_persisted(
        project.path(),
        "st_01d00003",
        json!({ "resolved_model": with_fields(resolved_model_fixture(), json!({ "capabilities": ["vision"] })) }),
    );
    let result = list(project.path());
    assert_eq!(result.diagnostics, Vec::new());
    assert_eq!(
        serde_json::to_value(&result.records[0].resolved_model).expect("json"),
        resolved_model_fixture()
    );
}

fn assert_exact_parse_error(project: &Path, path: PathBuf, message: &str) {
    let result = list(project);
    assert_eq!(result.records, Vec::new());
    assert_eq!(
        result.diagnostics,
        vec![TaskRecordDiagnostic::ParseError {
            path,
            message: message.into()
        }]
    );
}

#[test]
fn security_malformed_resolved_model_source_is_typed_diagnostic() {
    let project = project();
    let path = write_persisted(
        project.path(),
        "st_01d00004",
        json!({ "resolved_model": with_fields(resolved_model_fixture(), json!({ "source": "assistant" })) }),
    );
    assert_exact_parse_error(
        project.path(),
        path,
        "resolved_model.source must be category or explicit or agent",
    );
}

#[test]
fn security_malformed_known_resolved_model_fields_are_typed_diagnostics() {
    let cases = [
        (
            "st_bad0a01",
            json!({ "provider": 1234 }),
            "provider is not a string",
        ),
        (
            "st_bad0a02",
            json!({ "model_id": 1234 }),
            "model_id is not a string",
        ),
        (
            "st_bad0a03",
            json!({ "display": 1234 }),
            "display is not a string",
        ),
        (
            "st_bad0a04",
            json!({ "reasoning_effort": 1234 }),
            "reasoning_effort is not a string",
        ),
    ];
    for (task_id, fields, message) in cases {
        let project = project();
        let base = with_fields(
            resolved_model_fixture(),
            json!({ "reasoning_effort": "medium", "source": "explicit" }),
        );
        let path = write_persisted(
            project.path(),
            task_id,
            json!({ "resolved_model": with_fields(base, fields) }),
        );
        assert_exact_parse_error(project.path(), path, message);
    }
}

#[test]
fn security_spawn_extensions_and_member_env_are_discarded() {
    let project = project();
    write_persisted(
        project.path(),
        "st_01d00006",
        json!({ "spawn_spec": { "cwd": "/safe/project", "extensions": ["/tmp/malicious-extension.ts"], "member_env": { "MALICIOUS_MEMBER_ENV": "execute-me" } } }),
    );
    let result = list(project.path());
    assert_eq!(result.diagnostics, Vec::new());
    assert_eq!(
        serde_json::to_value(&result.records[0].spawn_spec).expect("json"),
        json!({ "cwd": "/safe/project" })
    );
}

#[test]
fn security_killed_fact_survives_parse() {
    let project = project();
    write_persisted(
        project.path(),
        "st_deadbee1",
        json!({ "status": "error", "killed": true, "error_message": "RPC child killed by signal SIGKILL" }),
    );
    let result = list(project.path());
    assert_eq!(result.diagnostics, Vec::new());
    assert_eq!(result.records[0].killed, Some(true));
}

#[test]
fn security_non_boolean_killed_is_typed_diagnostic() {
    let project = project();
    let path = write_persisted(project.path(), "st_deadbee2", json!({ "killed": "yes" }));
    assert_exact_parse_error(project.path(), path, "killed is not a boolean");
}

#[test]
fn security_malformed_pid_is_typed_diagnostic() {
    let project = project();
    let path = write_persisted(
        project.path(),
        "st_bad00001",
        json!({ "pid": "not-a-number" }),
    );
    assert_exact_parse_error(project.path(), path, "pid is not a number");
}

#[test]
fn security_malformed_tool_allow_is_typed_diagnostic() {
    let project = project();
    let path = write_persisted(
        project.path(),
        "st_bad00002",
        json!({ "tool_allow": ["read", 1234] }),
    );
    assert_exact_parse_error(project.path(), path, "tool_allow is not a string array");
}

#[test]
fn security_invalid_enum_value_is_redacted() {
    let project = project();
    write_persisted(
        project.path(),
        "st_bad00003",
        json!({ "status": "invalid-enum-sentinel" }),
    );
    let result = list(project.path());
    assert_eq!(result.records, Vec::new());
    let errors = parse_errors(&result);
    assert!(errors[0].1.contains("[REDACTED]"));
    assert!(!errors[0].1.contains("invalid-enum-sentinel"));
}

// The TS test spreads prompt/messages extras into the input object; the typed Rust input cannot
// carry undeclared fields, so the check is that the persisted file holds none of that text.
#[test]
fn security_runtime_extras_are_not_persisted() {
    let project = project();
    let record = create_task_record(input(), None).expect("id");
    store_for(project.path()).save(&record).expect("save");
    let persisted =
        std::fs::read_to_string(tasks_dir(project.path()).join(format!("{}.json", record.task_id)))
            .expect("read");
    assert!(!persisted.contains("prompt"));
    assert!(!persisted.contains("messages"));
}
