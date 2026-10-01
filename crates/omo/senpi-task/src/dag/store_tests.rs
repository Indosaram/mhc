//! `dag/store.test.ts`

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering::SeqCst;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize};
use std::sync::{Arc, Mutex, OnceLock};

use pretty_assertions::assert_eq;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use tempfile::TempDir;

use crate::dag::store::{
    DagEventPage, DagEventReadOptions, DagFileStore, DagFs, DagKeyRecord, DagNowFn,
    DagSettingsOverrides, DagStoreConfig, DagStoreError, DagStoreOptions, DagStorePaths,
    DagStoreTaskConfig, IsProcessAliveFn, RealDagFs, create_dag_file_store, resolve_dag_state_dir,
};
use crate::dag::types::{
    DagEventLane, DagNodeCounts, DagRunEvent, DagRunEventPayload, DagRunEventType, SchemaVersion1,
};

const RUN_ID: &str = "run-1";
const OTHER_RUN_ID: &str = "run-2";

type CreateHook = Box<dyn Fn(&Path) -> io::Result<File> + Send + Sync>;
type FsyncHook = Box<dyn Fn(&File) -> io::Result<()> + Send + Sync>;
type PairHook = Box<dyn Fn(&Path, &Path) -> io::Result<()> + Send + Sync>;

/// The Rust counterpart of `spyOn(fs, ...)`: each hook replaces one primitive, others stay real.
#[derive(Default)]
struct HookFs {
    create_new: Option<CreateHook>,
    fsync: Option<FsyncHook>,
    rename: Option<PairHook>,
    hard_link: Option<PairHook>,
}

impl DagFs for HookFs {
    fn create_new(&self, path: &Path) -> io::Result<File> {
        match &self.create_new {
            Some(hook) => hook(path),
            None => RealDagFs.create_new(path),
        }
    }

    fn fsync(&self, file: &File) -> io::Result<()> {
        match &self.fsync {
            Some(hook) => hook(file),
            None => RealDagFs.fsync(file),
        }
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        match &self.rename {
            Some(hook) => hook(from, to),
            None => RealDagFs.rename(from, to),
        }
    }

    fn hard_link(&self, existing: &Path, new: &Path) -> io::Result<()> {
        match &self.hard_link {
            Some(hook) => hook(existing, new),
            None => RealDagFs.hard_link(existing, new),
        }
    }
}

fn on_create(hook: impl Fn(&Path) -> io::Result<File> + Send + Sync + 'static) -> Option<CreateHook> {
    Some(Box::new(hook))
}

fn on_fsync(hook: impl Fn(&File) -> io::Result<()> + Send + Sync + 'static) -> Option<FsyncHook> {
    Some(Box::new(hook))
}

fn on_pair(hook: impl Fn(&Path, &Path) -> io::Result<()> + Send + Sync + 'static) -> Option<PairHook> {
    Some(Box::new(hook))
}

fn alive_fn(alive: impl Fn(i64) -> bool + Send + Sync + 'static) -> Option<IsProcessAliveFn> {
    Some(Arc::new(alive))
}

fn stepping_clock() -> Option<DagNowFn> {
    let clock = Arc::new(AtomicI64::new(0));
    Some(Arc::new(move || clock.fetch_add(1_001, SeqCst) + 1_001))
}

fn temp_project() -> TempDir {
    tempfile::tempdir().expect("temp project")
}

fn config(project: &Path) -> DagStoreConfig {
    DagStoreConfig::new(project)
}

fn config_with(project: &Path, dag: DagSettingsOverrides) -> DagStoreConfig {
    DagStoreConfig {
        project_dir: project.to_path_buf(),
        task: Some(DagStoreTaskConfig {
            state_dir: None,
            dag: Some(dag),
        }),
    }
}

fn open(project: &Path, options: DagStoreOptions) -> DagFileStore {
    create_dag_file_store(&config(project), options).expect("store opens")
}

fn run_lock_path(project: &Path) -> PathBuf {
    DagStorePaths::new(&resolve_dag_state_dir(&config(project))).run_lock(RUN_ID)
}

fn own_pid() -> i64 {
    i64::from(std::process::id())
}

fn event(seq: u64) -> DagRunEvent {
    DagRunEvent {
        schema_version: SchemaVersion1,
        run_id: RUN_ID.to_string(),
        seq,
        at: format!("2026-01-01T00:00:{seq:02}.000Z"),
        lane: DagEventLane::Boundary,
        payload: DagRunEventPayload::RunStarted { generation: seq },
    }
}

fn completed_event(seq: u64) -> DagRunEvent {
    DagRunEvent {
        payload: DagRunEventPayload::RunCompleted {
            counts: DagNodeCounts::default(),
        },
        ..event(seq)
    }
}

fn checkpoint(id: &str, status: &str, completed_at: Option<&str>, task_id: Option<&str>) -> Value {
    let mut record = Map::new();
    record.insert("schemaVersion".to_string(), json!(1));
    record.insert("runId".to_string(), json!(id));
    record.insert("runKey".to_string(), json!(format!("key-{id}")));
    record.insert("parentSessionId".to_string(), json!("parent-session"));
    record.insert("status".to_string(), json!(status));
    if let Some(at) = completed_at {
        record.insert("completedAt".to_string(), json!(at));
    }
    record.insert(
        "nodes".to_string(),
        task_id.map_or_else(|| json!([]), |task_id| json!([{ "taskId": task_id }])),
    );
    Value::Object(record)
}

fn limit(limit: usize) -> DagEventReadOptions {
    DagEventReadOptions {
        limit,
        ..DagEventReadOptions::default()
    }
}

fn seqs(page: &DagEventPage) -> Vec<u64> {
    page.events.iter().map(|event| event.seq).collect()
}

fn page_meta(page: &DagEventPage) -> (u64, u64, bool) {
    (page.next_since_seq, page.head_seq, page.has_more)
}

fn err_of<T>(result: Result<T, DagStoreError>) -> DagStoreError {
    match result {
        Ok(_) => panic!("expected an error"),
        Err(error) => error,
    }
}

fn assert_journal_corrupt(error: DagStoreError, run_id: &str) {
    match error {
        DagStoreError::JournalCorrupt(error) => {
            assert_eq!(error.diagnostic.kind(), "journal_corrupt");
            assert_eq!(error.diagnostic.run_id(), Some(run_id));
        }
        other => panic!("expected DagJournalCorruptError, got {other:?}"),
    }
}

fn timeout_message(path: &Path) -> String {
    format!("Timed out acquiring DAG lock: {}", path.display())
}

fn has_owner_record(value: &Value) -> bool {
    value.get("hostPid").is_some_and(Value::is_number)
        && value.get("token").is_some_and(Value::is_string)
}

fn flag() -> Arc<AtomicBool> {
    Arc::new(AtomicBool::new(false))
}

// createDagFileStore event WAL

#[test]
fn given_five_durable_events_when_reading_two_at_a_time_then_since_seq_is_exclusive_and_has_more_flips() {
    // given
    let project = temp_project();
    let store = open(project.path(), DagStoreOptions::default());
    for seq in 1..=5 {
        store.append_event(&event(seq)).expect("append");
    }

    // when
    let first = store.read_events(RUN_ID, 0, &limit(2)).expect("first");
    let second = store.read_events(RUN_ID, first.next_since_seq, &limit(2)).expect("second");
    let third = store.read_events(RUN_ID, second.next_since_seq, &limit(2)).expect("third");

    // then
    assert_eq!(seqs(&first), vec![1, 2]);
    assert_eq!(page_meta(&first), (2, 5, true));
    assert_eq!(seqs(&second), vec![3, 4]);
    assert_eq!(page_meta(&second), (4, 5, true));
    assert_eq!(seqs(&third), vec![5]);
    assert_eq!(page_meta(&third), (5, 5, false));
}

#[test]
fn given_mixed_events_beyond_a_catch_up_boundary_when_filtering_a_bounded_page_then_lane_type_and_through_seq_all_apply() {
    // given
    let project = temp_project();
    let store = open(project.path(), DagStoreOptions::default());
    store.append_event(&event(1)).expect("append 1");
    store
        .append_event(&DagRunEvent {
            lane: DagEventLane::Activity,
            ..event(2)
        })
        .expect("append 2");
    store.append_event(&completed_event(3)).expect("append 3");
    store.append_event(&completed_event(4)).expect("append 4");

    // when
    let page = store
        .read_events(
            RUN_ID,
            1,
            &DagEventReadOptions {
                limit: 10,
                lane: Some(DagEventLane::Boundary),
                types: Some(vec![DagRunEventType::RunCompleted]),
                through_seq: Some(3),
            },
        )
        .expect("page");

    // then
    assert_eq!(seqs(&page), vec![3]);
    assert_eq!(page_meta(&page), (3, 4, false));
}

#[test]
fn given_an_existing_event_seq_when_a_distinct_event_reuses_it_then_the_wal_rejects_the_duplicate() {
    // given
    let project = temp_project();
    let store = open(project.path(), DagStoreOptions::default());
    store.append_event(&event(1)).expect("append");

    // when
    let duplicate = store.append_event(&DagRunEvent {
        payload: DagRunEventPayload::RunStarted { generation: 2 },
        ..event(1)
    });

    // then
    let message = err_of(duplicate).to_string();
    assert!(
        message.contains("DAG event seq must be strictly increasing"),
        "{message}"
    );
    assert_eq!(
        store.read_events(RUN_ID, 0, &limit(10)).expect("read").events.len(),
        1
    );
}

#[test]
fn given_default_store_durability_when_one_event_is_appended_then_the_wal_descriptor_is_fsynced() {
    // given
    let project = temp_project();
    let count = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&count);
    let hooks = HookFs {
        fsync: on_fsync(move |file: &File| {
            seen.fetch_add(1, SeqCst);
            RealDagFs.fsync(file)
        }),
        ..HookFs::default()
    };
    let store = open(
        project.path(),
        DagStoreOptions {
            fs: Some(Arc::new(hooks)),
            ..DagStoreOptions::default()
        },
    );

    // when
    store.append_event(&event(1)).expect("append");

    // then
    assert_eq!(count.load(SeqCst), 1);
}

#[test]
fn given_fsync_is_disabled_when_wal_lock_and_checkpoint_writes_run_then_ordering_remains_without_durability_barriers() {
    // given
    let project = temp_project();
    let count = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&count);
    let hooks = HookFs {
        fsync: on_fsync(move |file: &File| {
            seen.fetch_add(1, SeqCst);
            RealDagFs.fsync(file)
        }),
        ..HookFs::default()
    };
    let store = open(
        project.path(),
        DagStoreOptions {
            fsync: Some(false),
            fs: Some(Arc::new(hooks)),
            ..DagStoreOptions::default()
        },
    );

    // when
    store
        .with_run_lock(RUN_ID, || -> Result<(), DagStoreError> {
            store.append_event(&event(1))?;
            store.write_checkpoint(
                RUN_ID,
                &json!({ "schemaVersion": 1, "runId": RUN_ID, "checkpointSeq": 1 }),
            )
        })
        .expect("lock")
        .expect("writes");

    // then
    assert_eq!(count.load(SeqCst), 0);
    assert_eq!(
        seqs(&store.read_events(RUN_ID, 0, &limit(10)).expect("read")),
        vec![1]
    );
    let stored = store
        .read_checkpoint::<Value>(RUN_ID)
        .expect("read checkpoint")
        .expect("checkpoint exists");
    assert_eq!(stored["checkpointSeq"], json!(1));
}

#[test]
fn given_a_wal_ending_in_a_torn_fragment_when_a_new_store_opens_then_valid_events_remain_and_the_tail_is_diagnosed_and_discarded() {
    // given
    let project = temp_project();
    let writer = open(project.path(), DagStoreOptions::default());
    writer.append_event(&event(1)).expect("append 1");
    writer.append_event(&event(2)).expect("append 2");
    let mut file = OpenOptions::new()
        .append(true)
        .open(writer.paths.event(RUN_ID))
        .expect("open wal");
    file.write_all(br#"{"schemaVersion":1,"runId":"run-1","seq":3"#)
        .expect("torn write");
    drop(file);

    // when
    let recovered = open(project.path(), DagStoreOptions::default());
    let page = recovered.read_events(RUN_ID, 0, &limit(10)).expect("read");

    // then
    assert_eq!(seqs(&page), vec![1, 2]);
    let diagnostics = recovered.diagnostics();
    assert_eq!(diagnostics.len(), 1);
    assert_eq!(diagnostics[0].kind(), "event_log_recovered");
    assert_eq!(diagnostics[0].run_id(), Some(RUN_ID));
    let content = fs::read_to_string(recovered.paths.event(RUN_ID)).expect("wal");
    assert!(content.ends_with('\n'), "{content}");
}

#[test]
fn given_a_future_schema_event_file_when_a_store_opens_then_it_fails_closed_with_a_journal_corrupt_diagnostic() {
    // given
    let project = temp_project();
    let seeded = open(project.path(), DagStoreOptions::default());
    let mut value = serde_json::to_value(event(1)).expect("event json");
    value["schemaVersion"] = json!(2);
    fs::write(seeded.paths.event(RUN_ID), format!("{value}\n")).expect("seed");

    // when
    let opened = create_dag_file_store(&config(project.path()), DagStoreOptions::default());

    // then
    assert_journal_corrupt(err_of(opened), RUN_ID);
}

// createDagFileStore checkpoints and layout

#[test]
fn given_posix_checkpoint_persistence_when_an_existing_checkpoint_is_replaced_then_readers_see_the_old_complete_file_until_rename_and_both_file_and_directory_are_fsynced() {
    // given
    let project = temp_project();
    let order = Arc::new(Mutex::new(Vec::<String>::new()));
    let observed = Arc::new(Mutex::new(Vec::<Value>::new()));
    let fsync_count = Arc::new(AtomicUsize::new(0));
    let hooks = HookFs {
        fsync: on_fsync({
            let order = Arc::clone(&order);
            let fsync_count = Arc::clone(&fsync_count);
            move |_file: &File| {
                fsync_count.fetch_add(1, SeqCst);
                order.lock().unwrap().push("fsync".to_string());
                Ok(())
            }
        }),
        rename: on_pair({
            let order = Arc::clone(&order);
            let observed = Arc::clone(&observed);
            move |from: &Path, to: &Path| {
                order.lock().unwrap().push("rename".to_string());
                let previous: Value =
                    serde_json::from_str(&fs::read_to_string(to).expect("old checkpoint"))
                        .expect("old checkpoint json");
                observed.lock().unwrap().push(previous);
                RealDagFs.rename(from, to)
            }
        }),
        ..HookFs::default()
    };
    let store = open(
        project.path(),
        DagStoreOptions {
            is_process_alive: alive_fn(|_| true),
            platform: Some("darwin".to_string()),
            fs: Some(Arc::new(hooks)),
            ..DagStoreOptions::default()
        },
    );
    fs::write(
        store.paths.run(RUN_ID),
        json!({ "schemaVersion": 1, "runId": RUN_ID, "generation": 1 }).to_string(),
    )
    .expect("seed");

    // when
    store
        .write_checkpoint(RUN_ID, &json!({ "schemaVersion": 1, "runId": RUN_ID, "generation": 2 }))
        .expect("write");

    // then
    assert_eq!(
        *observed.lock().unwrap(),
        vec![json!({ "schemaVersion": 1, "runId": RUN_ID, "generation": 1 })]
    );
    let stored = store
        .read_checkpoint::<Value>(RUN_ID)
        .expect("read")
        .expect("exists");
    assert_eq!(stored["generation"], json!(2));
    assert_eq!(fsync_count.load(SeqCst), 2);
    assert_eq!(*order.lock().unwrap(), vec!["fsync", "rename", "fsync"]);
}

#[test]
fn given_windows_checkpoint_persistence_when_directory_fsync_would_fail_with_eperm_then_the_atomic_checkpoint_still_succeeds() {
    // given
    let project = temp_project();
    let fsync_count = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&fsync_count);
    let hooks = HookFs {
        fsync: on_fsync(move |_file: &File| {
            if seen.fetch_add(1, SeqCst) == 1 {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "operation not permitted",
                ));
            }
            Ok(())
        }),
        ..HookFs::default()
    };
    let store = open(
        project.path(),
        DagStoreOptions {
            is_process_alive: alive_fn(|_| true),
            platform: Some("win32".to_string()),
            fs: Some(Arc::new(hooks)),
            ..DagStoreOptions::default()
        },
    );

    // when
    let write = store.write_checkpoint(
        RUN_ID,
        &json!({ "schemaVersion": 1, "runId": RUN_ID, "generation": 1 }),
    );

    // then
    assert!(write.is_ok(), "{write:?}");
    assert_eq!(fsync_count.load(SeqCst), 1);
    let stored = store
        .read_checkpoint::<Value>(RUN_ID)
        .expect("read")
        .expect("exists");
    assert_eq!(stored["generation"], json!(1));
}

#[test]
fn given_a_future_schema_checkpoint_when_it_is_opened_then_it_fails_closed_with_a_journal_corrupt_diagnostic() {
    // given
    let project = temp_project();
    let store = open(project.path(), DagStoreOptions::default());
    fs::write(
        store.paths.run(RUN_ID),
        json!({ "schemaVersion": 2, "runId": RUN_ID }).to_string(),
    )
    .expect("seed");

    // when
    let read = store.read_checkpoint::<Value>(RUN_ID);

    // then
    assert_journal_corrupt(err_of(read), RUN_ID);
}

#[test]
fn given_an_existing_node_result_when_it_is_replaced_then_readers_see_the_old_complete_file_until_rename_and_both_file_and_directory_are_fsynced() {
    // given
    let project = temp_project();
    let order = Arc::new(Mutex::new(Vec::<String>::new()));
    let observed = Arc::new(Mutex::new(Vec::<String>::new()));
    let fsync_count = Arc::new(AtomicUsize::new(0));
    let hooks = HookFs {
        fsync: on_fsync({
            let order = Arc::clone(&order);
            let fsync_count = Arc::clone(&fsync_count);
            move |_file: &File| {
                fsync_count.fetch_add(1, SeqCst);
                order.lock().unwrap().push("fsync".to_string());
                Ok(())
            }
        }),
        rename: on_pair({
            let order = Arc::clone(&order);
            let observed = Arc::clone(&observed);
            move |from: &Path, to: &Path| {
                order.lock().unwrap().push("rename".to_string());
                if to.exists() {
                    observed
                        .lock()
                        .unwrap()
                        .push(fs::read_to_string(to).expect("old result"));
                }
                RealDagFs.rename(from, to)
            }
        }),
        ..HookFs::default()
    };
    let store = open(
        project.path(),
        DagStoreOptions {
            is_process_alive: alive_fn(|_| true),
            platform: Some("linux".to_string()),
            fs: Some(Arc::new(hooks)),
            ..DagStoreOptions::default()
        },
    );
    store.write_result(RUN_ID, "node-a", "old result").expect("old");
    observed.lock().unwrap().clear();
    order.lock().unwrap().clear();
    fsync_count.store(0, SeqCst);

    // when
    store.write_result(RUN_ID, "node-a", "new result").expect("new");

    // then
    assert_eq!(*observed.lock().unwrap(), vec!["old result"]);
    assert_eq!(
        store.read_result(RUN_ID, "node-a").expect("read"),
        Some("new result".to_string())
    );
    assert_eq!(fsync_count.load(SeqCst), 2);
    assert_eq!(*order.lock().unwrap(), vec!["fsync", "rename", "fsync"]);
}

#[test]
fn given_a_one_run_session_limit_when_a_second_run_checkpoint_is_created_then_the_configured_limit_rejects_it_without_an_orphan() {
    // given
    let project = temp_project();
    let store = create_dag_file_store(
        &config_with(
            project.path(),
            DagSettingsOverrides {
                max_runs_per_session: Some(1),
                ..DagSettingsOverrides::default()
            },
        ),
        DagStoreOptions::default(),
    )
    .expect("store opens");
    store
        .write_checkpoint(RUN_ID, &checkpoint(RUN_ID, "running", None, None))
        .expect("first");

    // when
    let second = store.write_checkpoint(
        OTHER_RUN_ID,
        &checkpoint(OTHER_RUN_ID, "running", None, None),
    );

    // then
    let message = err_of(second).to_string();
    assert!(message.contains("DAG session run limit reached: 1"), "{message}");
    assert_eq!(
        store.read_checkpoint::<Value>(OTHER_RUN_ID).expect("read"),
        None
    );
    let mut names: Vec<String> = fs::read_dir(&store.paths.runs)
        .expect("runs dir")
        .map(|entry| entry.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names, vec![format!("{RUN_ID}.json")]);
}

#[test]
fn given_a_parent_session_and_run_key_when_writing_the_key_then_its_filename_is_the_exact_nul_delimited_sha256() {
    // given
    let project = temp_project();
    let store = open(project.path(), DagStoreOptions::default());
    let parent_session_id = "parent/session";
    let run_key = "release-plan";
    let expected_hash: String = Sha256::digest(format!("{parent_session_id}\0{run_key}").as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();

    // when
    let path = store
        .write_key(&DagKeyRecord {
            schema_version: SchemaVersion1,
            parent_session_id: parent_session_id.to_string(),
            run_key: run_key.to_string(),
            run_id: RUN_ID.to_string(),
            definition_fingerprint: None,
        })
        .expect("write key");

    // then
    assert_eq!(path, store.paths.keys.join(format!("{expected_hash}.json")));
    assert_eq!(
        store
            .read_key(parent_session_id, run_key)
            .expect("read key")
            .map(|record| record.run_id),
        Some(RUN_ID.to_string())
    );
}

// createDagFileStore locks and retention

#[test]
fn given_a_lock_held_by_a_dead_pid_when_the_run_lock_is_acquired_then_the_stale_lock_is_reclaimed() {
    // given
    let project = temp_project();
    let store = open(
        project.path(),
        DagStoreOptions {
            is_process_alive: alive_fn(|_| false),
            ..DagStoreOptions::default()
        },
    );
    let canonical = store.paths.run_lock(RUN_ID);
    fs::write(&canonical, json!({ "hostPid": 2_147_483_647 }).to_string()).expect("seed");
    let mut entered = false;

    // when
    store
        .with_run_lock(RUN_ID, || {
            entered = true;
            assert!(canonical.exists());
        })
        .expect("lock");

    // then
    assert!(entered);
    assert!(!canonical.exists());
}

#[test]
fn given_a_concurrent_observer_watches_stale_reclamation_when_ownership_changes_then_the_canonical_lock_path_is_never_vacant() {
    // given
    let project = temp_project();
    let canonical = run_lock_path(project.path());
    let observed_presence = Arc::new(Mutex::new(Vec::<bool>::new()));
    let observing = Arc::new(AtomicBool::new(true));
    let hooks = HookFs {
        rename: on_pair({
            let canonical = canonical.clone();
            let observed_presence = Arc::clone(&observed_presence);
            let observing = Arc::clone(&observing);
            move |from: &Path, to: &Path| {
                RealDagFs.rename(from, to)?;
                if observing.load(SeqCst)
                    && (from == canonical.as_path() || to == canonical.as_path())
                {
                    observed_presence.lock().unwrap().push(canonical.exists());
                }
                Ok(())
            }
        }),
        ..HookFs::default()
    };
    let store = open(
        project.path(),
        DagStoreOptions {
            is_process_alive: alive_fn(|_| false),
            fs: Some(Arc::new(hooks)),
            ..DagStoreOptions::default()
        },
    );
    fs::write(
        &canonical,
        json!({ "hostPid": 2_147_483_647, "token": "stale" }).to_string(),
    )
    .expect("seed");

    // when
    store
        .with_run_lock(RUN_ID, || {
            observed_presence.lock().unwrap().push(canonical.exists());
            observing.store(false, SeqCst);
        })
        .expect("lock");

    // then
    let presence = observed_presence.lock().unwrap().clone();
    assert!(!presence.is_empty());
    assert!(!presence.contains(&false), "{presence:?}");
}

#[test]
fn given_two_reclaimers_validate_one_stale_holder_when_they_contend_at_publication_then_reclamation_stays_exclusive() {
    // given
    const CONTENDER_HOLDER: &str = r#"{"hostPid":303,"owner":"contender"}"#;
    let project = temp_project();
    let canonical = run_lock_path(project.path());
    let contender_pid = 303;
    let host = own_pid();
    let is_alive = move |pid: i64| pid == host || pid == contender_pid;
    let contender_cell: Arc<OnceLock<DagFileStore>> = Arc::new(OnceLock::new());
    let probing = flag();
    let publication_probed = flag();
    let contender_entered = flag();
    let contender_live = flag();
    let vacancy_observed = flag();
    let restore_overwrote_contender = flag();
    let hooks = HookFs {
        rename: on_pair({
            let canonical = canonical.clone();
            let contender_cell = Arc::clone(&contender_cell);
            let probing = Arc::clone(&probing);
            let publication_probed = Arc::clone(&publication_probed);
            let contender_entered = Arc::clone(&contender_entered);
            let contender_live = Arc::clone(&contender_live);
            let vacancy_observed = Arc::clone(&vacancy_observed);
            let restore_overwrote_contender = Arc::clone(&restore_overwrote_contender);
            move |from: &Path, to: &Path| {
                if !probing.load(SeqCst)
                    && !publication_probed.load(SeqCst)
                    && from.to_string_lossy().ends_with(".successor")
                    && to == canonical.as_path()
                {
                    probing.store(true, SeqCst);
                    publication_probed.store(true, SeqCst);
                    let contender = contender_cell.get().expect("contender store");
                    let result = contender.with_run_lock(RUN_ID, || {
                        contender_entered.store(true, SeqCst);
                        contender_live.store(true, SeqCst);
                        fs::write(&canonical, CONTENDER_HOLDER).expect("contender write");
                    });
                    if let Err(error) = result {
                        let message = error.to_string();
                        assert!(message.contains("Timed out acquiring DAG lock"), "{message}");
                    }
                    if !canonical.exists() {
                        vacancy_observed.store(true, SeqCst);
                    }
                    RealDagFs.rename(from, to)?;
                    if contender_live.load(SeqCst) {
                        restore_overwrote_contender.store(true, SeqCst);
                    }
                    probing.store(false, SeqCst);
                    return Ok(());
                }
                RealDagFs.rename(from, to)
            }
        }),
        ..HookFs::default()
    };
    let shared_fs: Arc<dyn DagFs> = Arc::new(hooks);
    let reclaimer = open(
        project.path(),
        DagStoreOptions {
            now: stepping_clock(),
            is_process_alive: alive_fn(is_alive),
            fs: Some(Arc::clone(&shared_fs)),
            ..DagStoreOptions::default()
        },
    );
    let contender_store = open(
        project.path(),
        DagStoreOptions {
            now: stepping_clock(),
            is_process_alive: alive_fn(is_alive),
            fs: Some(Arc::clone(&shared_fs)),
            ..DagStoreOptions::default()
        },
    );
    assert!(contender_cell.set(contender_store).is_ok());
    fs::write(&canonical, r#"{"hostPid":101,"owner":"stale"}"#).expect("seed");
    let mut overlapping_live_holders_possible = false;

    // when
    reclaimer
        .with_run_lock(RUN_ID, || {
            overlapping_live_holders_possible |= contender_live.load(SeqCst);
        })
        .expect("reclaimer lock");
    if !contender_entered.load(SeqCst) {
        contender_cell
            .get()
            .expect("contender store")
            .with_run_lock(RUN_ID, || {
                contender_entered.store(true, SeqCst);
                contender_live.store(true, SeqCst);
                fs::write(&canonical, CONTENDER_HOLDER).expect("contender write");
            })
            .expect("contender lock");
    }
    let final_owner = if canonical.exists() {
        let value: Value =
            serde_json::from_str(&fs::read_to_string(&canonical).expect("final holder"))
                .expect("final holder json");
        value.get("owner").and_then(Value::as_str).map(str::to_string)
    } else {
        None
    };

    // then
    assert!(publication_probed.load(SeqCst));
    assert_eq!(
        json!({
            "vacancyObserved": vacancy_observed.load(SeqCst),
            "contenderSurvived": final_owner.as_deref() == Some("contender"),
            "restoreOverwroteContender": restore_overwrote_contender.load(SeqCst),
            "overlappingLiveHoldersPossible": overlapping_live_holders_possible,
        }),
        json!({
            "vacancyObserved": false,
            "contenderSurvived": true,
            "restoreOverwroteContender": false,
            "overlappingLiveHoldersPossible": false,
        })
    );
}

#[test]
fn given_a_second_reclaimer_observes_sentinel_initialization_when_the_first_is_preempted_after_open_then_no_ownerless_sentinel_is_visible() {
    // given
    let project = temp_project();
    let canonical = run_lock_path(project.path());
    let sentinel_text = format!("{}.reclaim", canonical.to_string_lossy());
    let reclaim_sentinel = PathBuf::from(&sentinel_text);
    let host = own_pid();
    let second_cell: Arc<OnceLock<DagFileStore>> = Arc::new(OnceLock::new());
    let preempted = flag();
    let second_reclaimer_entered = flag();
    let sentinel_absent = flag();
    let ownerless_sentinel_observed = flag();
    let published_sentinel_had_owner_record = flag();
    let hooks = HookFs {
        hard_link: on_pair({
            let reclaim_sentinel = reclaim_sentinel.clone();
            let published = Arc::clone(&published_sentinel_had_owner_record);
            move |existing: &Path, new: &Path| {
                RealDagFs.hard_link(existing, new)?;
                if new != reclaim_sentinel.as_path() {
                    return Ok(());
                }
                let value: Value = serde_json::from_str(
                    &fs::read_to_string(&reclaim_sentinel).expect("published sentinel"),
                )
                .expect("published sentinel json");
                published.store(has_owner_record(&value), SeqCst);
                Ok(())
            }
        }),
        create_new: on_create({
            let reclaim_sentinel = reclaim_sentinel.clone();
            let second_cell = Arc::clone(&second_cell);
            let preempted = Arc::clone(&preempted);
            let entered = Arc::clone(&second_reclaimer_entered);
            let sentinel_absent = Arc::clone(&sentinel_absent);
            let ownerless = Arc::clone(&ownerless_sentinel_observed);
            move |path: &Path| {
                let file = RealDagFs.create_new(path)?;
                if preempted.load(SeqCst) || !path.to_string_lossy().starts_with(&sentinel_text) {
                    return Ok(file);
                }
                preempted.store(true, SeqCst);
                let absent = !reclaim_sentinel.exists();
                sentinel_absent.store(absent, SeqCst);
                if !absent {
                    let content =
                        fs::read_to_string(&reclaim_sentinel).expect("sentinel content");
                    match serde_json::from_str::<Value>(&content) {
                        Ok(value) => ownerless.store(!has_owner_record(&value), SeqCst),
                        Err(_) => ownerless.store(true, SeqCst),
                    }
                }
                second_cell
                    .get()
                    .expect("second store")
                    .with_run_lock(RUN_ID, || entered.store(true, SeqCst))
                    .expect("second reclaimer lock");
                Ok(file)
            }
        }),
        ..HookFs::default()
    };
    let shared_fs: Arc<dyn DagFs> = Arc::new(hooks);
    let first = open(
        project.path(),
        DagStoreOptions {
            is_process_alive: alive_fn(move |pid| pid == host),
            fs: Some(Arc::clone(&shared_fs)),
            ..DagStoreOptions::default()
        },
    );
    let second = open(
        project.path(),
        DagStoreOptions {
            is_process_alive: alive_fn(move |pid| pid == host),
            fs: Some(Arc::clone(&shared_fs)),
            ..DagStoreOptions::default()
        },
    );
    assert!(second_cell.set(second).is_ok());
    fs::write(
        &canonical,
        json!({ "hostPid": 101, "token": "stale-holder" }).to_string(),
    )
    .expect("seed");

    // when
    first.with_run_lock(RUN_ID, || {}).expect("first lock");

    // then
    assert_eq!(
        json!({
            "preempted": preempted.load(SeqCst),
            "secondReclaimerEntered": second_reclaimer_entered.load(SeqCst),
            "sentinelAbsentDuringInitialization": sentinel_absent.load(SeqCst),
            "ownerlessSentinelObserved": ownerless_sentinel_observed.load(SeqCst),
            "publishedSentinelHadOwnerRecord": published_sentinel_had_owner_record.load(SeqCst),
        }),
        json!({
            "preempted": true,
            "secondReclaimerEntered": true,
            "sentinelAbsentDuringInitialization": true,
            "ownerlessSentinelObserved": false,
            "publishedSentinelHadOwnerRecord": true,
        })
    );
}

#[test]
fn given_a_crashed_reclaimer_left_its_sentinel_when_another_process_reclaims_the_stale_holder_then_the_sentinel_cannot_wedge_acquisition() {
    // given
    let project = temp_project();
    let store = open(
        project.path(),
        DagStoreOptions {
            is_process_alive: alive_fn(|_| false),
            ..DagStoreOptions::default()
        },
    );
    let canonical = store.paths.run_lock(RUN_ID);
    let reclaim_sentinel = PathBuf::from(format!("{}.reclaim", canonical.to_string_lossy()));
    fs::write(
        &canonical,
        json!({ "hostPid": 101, "token": "stale-holder" }).to_string(),
    )
    .expect("seed holder");
    fs::write(
        &reclaim_sentinel,
        json!({ "hostPid": 202, "token": "crashed-reclaimer" }).to_string(),
    )
    .expect("seed sentinel");
    let mut entered = false;

    // when
    store
        .with_run_lock(RUN_ID, || {
            entered = true;
        })
        .expect("lock");

    // then
    assert!(entered);
    assert!(!reclaim_sentinel.exists());
}

#[test]
fn given_a_contender_probes_every_reclamation_transition_when_a_stale_lock_is_replaced_then_only_the_reclaimer_concludes_it_holds_the_lock() {
    // given
    let project = temp_project();
    let canonical = run_lock_path(project.path());
    let contender_cell: Arc<OnceLock<DagFileStore>> = Arc::new(OnceLock::new());
    let probed = flag();
    let contender_entered = flag();
    let contender_rejected = flag();
    let reclaimer_entered = flag();
    let hooks = HookFs {
        rename: on_pair({
            let canonical = canonical.clone();
            let contender_cell = Arc::clone(&contender_cell);
            let probed = Arc::clone(&probed);
            let contender_entered = Arc::clone(&contender_entered);
            let contender_rejected = Arc::clone(&contender_rejected);
            let reclaimer_entered = Arc::clone(&reclaimer_entered);
            move |from: &Path, to: &Path| {
                RealDagFs.rename(from, to)?;
                if !probed.load(SeqCst)
                    && !reclaimer_entered.load(SeqCst)
                    && (from == canonical.as_path() || to == canonical.as_path())
                {
                    probed.store(true, SeqCst);
                    let result = contender_cell
                        .get()
                        .expect("contender store")
                        .with_run_lock(RUN_ID, || contender_entered.store(true, SeqCst));
                    if let Err(error) = result {
                        contender_rejected.store(
                            error.to_string().contains("Timed out acquiring DAG lock"),
                            SeqCst,
                        );
                    }
                }
                Ok(())
            }
        }),
        ..HookFs::default()
    };
    let shared_fs: Arc<dyn DagFs> = Arc::new(hooks);
    let reclaimer = open(
        project.path(),
        DagStoreOptions {
            is_process_alive: alive_fn(|_| false),
            fs: Some(Arc::clone(&shared_fs)),
            ..DagStoreOptions::default()
        },
    );
    let contender = open(
        project.path(),
        DagStoreOptions {
            now: stepping_clock(),
            is_process_alive: alive_fn(|_| true),
            fs: Some(Arc::clone(&shared_fs)),
            ..DagStoreOptions::default()
        },
    );
    assert!(contender_cell.set(contender).is_ok());
    fs::write(
        &canonical,
        json!({ "hostPid": 2_147_483_647, "token": "stale" }).to_string(),
    )
    .expect("seed");

    // when
    reclaimer
        .with_run_lock(RUN_ID, || reclaimer_entered.store(true, SeqCst))
        .expect("reclaimer lock");

    // then
    assert!(probed.load(SeqCst));
    assert!(reclaimer_entered.load(SeqCst));
    assert!(contender_rejected.load(SeqCst));
    assert!(!contender_entered.load(SeqCst));
}

#[test]
fn given_a_fresh_holder_acquires_between_stale_inspection_and_removal_when_the_run_lock_retries_then_the_fresh_lock_survives_and_the_reclaimer_never_enters() {
    // given
    const FRESH: &str = r#"{"hostPid":202,"owner":"fresh"}"#;
    let project = temp_project();
    let canonical = run_lock_path(project.path());
    let fresh_pid = 202;
    let replaced = flag();
    let hooks = HookFs {
        create_new: on_create({
            let canonical = canonical.clone();
            let replaced = Arc::clone(&replaced);
            move |path: &Path| {
                let file = RealDagFs.create_new(path)?;
                if !replaced.load(SeqCst) && path.to_string_lossy().ends_with(".successor") {
                    replaced.store(true, SeqCst);
                    fs::write(&canonical, FRESH)?;
                }
                Ok(file)
            }
        }),
        ..HookFs::default()
    };
    let store = open(
        project.path(),
        DagStoreOptions {
            now: stepping_clock(),
            is_process_alive: alive_fn(move |pid| pid == fresh_pid),
            fs: Some(Arc::new(hooks)),
            ..DagStoreOptions::default()
        },
    );
    fs::write(&canonical, r#"{"hostPid":101,"owner":"stale"}"#).expect("seed");
    let mut entered = false;

    // when
    let acquire = store.with_run_lock(RUN_ID, || {
        entered = true;
    });

    // then
    assert_eq!(err_of(acquire).to_string(), timeout_message(&canonical));
    assert!(replaced.load(SeqCst));
    assert!(!entered);
    assert_eq!(fs::read_to_string(&canonical).expect("lock"), FRESH);
}

#[test]
fn given_a_contender_replaces_the_observed_stale_holder_before_atomic_takeover_when_validation_runs_then_the_contender_wins_and_the_reclaimer_never_enters() {
    // given
    const FRESH: &str = r#"{"hostPid":202,"owner":"fresh-before-quarantine"}"#;
    const CONTENDER: &str = r#"{"hostPid":303,"owner":"contender"}"#;
    let project = temp_project();
    let canonical = run_lock_path(project.path());
    let fresh_pid = 202;
    let contender_pid = 303;
    let successor_prepared = flag();
    let hooks = HookFs {
        create_new: on_create({
            let canonical = canonical.clone();
            let successor_prepared = Arc::clone(&successor_prepared);
            move |path: &Path| {
                let file = RealDagFs.create_new(path)?;
                if !successor_prepared.load(SeqCst)
                    && path.to_string_lossy().ends_with(".successor")
                {
                    successor_prepared.store(true, SeqCst);
                    fs::write(&canonical, FRESH)?;
                    fs::write(&canonical, CONTENDER)?;
                }
                Ok(file)
            }
        }),
        ..HookFs::default()
    };
    let store = open(
        project.path(),
        DagStoreOptions {
            now: stepping_clock(),
            is_process_alive: alive_fn(move |pid| pid == fresh_pid || pid == contender_pid),
            fs: Some(Arc::new(hooks)),
            ..DagStoreOptions::default()
        },
    );
    fs::write(&canonical, r#"{"hostPid":101,"owner":"stale"}"#).expect("seed");
    let mut entered = false;

    // when
    let acquire = store.with_run_lock(RUN_ID, || {
        entered = true;
    });

    // then
    assert_eq!(err_of(acquire).to_string(), timeout_message(&canonical));
    assert!(successor_prepared.load(SeqCst));
    assert!(!entered);
    assert_eq!(fs::read_to_string(&canonical).expect("lock"), CONTENDER);
}

#[test]
fn given_an_acquired_lock_path_is_replaced_before_release_when_the_original_holder_exits_then_it_never_unlinks_the_replacement() {
    // given
    const CONTENDER: &str = r#"{"hostPid":303,"owner":"contender"}"#;
    let project = temp_project();
    let store = open(project.path(), DagStoreOptions::default());
    let canonical = store.paths.run_lock(RUN_ID);

    // when
    store
        .with_run_lock(RUN_ID, || {
            fs::remove_file(&canonical).expect("remove lock");
            fs::write(&canonical, CONTENDER).expect("replace lock");
        })
        .expect("lock");

    // then
    assert_eq!(fs::read_to_string(&canonical).expect("lock"), CONTENDER);
}

#[test]
fn given_a_dead_holder_is_replaced_before_reclamation_when_the_run_lock_retries_then_it_never_deletes_the_fresh_holder() {
    // given
    let project = temp_project();
    let canonical = run_lock_path(project.path());
    let stale_pid = 101;
    let fresh_pid = 202;
    let replaced = flag();
    let store = open(
        project.path(),
        DagStoreOptions {
            now: stepping_clock(),
            is_process_alive: alive_fn({
                let canonical = canonical.clone();
                let replaced = Arc::clone(&replaced);
                move |pid| {
                    if pid == stale_pid && !replaced.load(SeqCst) {
                        replaced.store(true, SeqCst);
                        fs::write(&canonical, json!({ "hostPid": fresh_pid }).to_string())
                            .expect("replace holder");
                        return false;
                    }
                    pid == fresh_pid
                }
            }),
            ..DagStoreOptions::default()
        },
    );
    fs::write(&canonical, json!({ "hostPid": stale_pid }).to_string()).expect("seed");
    let mut entered = false;

    // when
    let acquire = store.with_run_lock(RUN_ID, || {
        entered = true;
    });

    // then
    assert_eq!(err_of(acquire).to_string(), timeout_message(&canonical));
    assert!(!entered);
    let holder: Value =
        serde_json::from_str(&fs::read_to_string(&canonical).expect("lock")).expect("lock json");
    assert_eq!(holder, json!({ "hostPid": fresh_pid }));
}

fn retention_now() -> i64 {
    chrono::DateTime::parse_from_rfc3339("2026-01-10T00:00:00.000Z")
        .expect("timestamp")
        .timestamp_millis()
}

fn retention_store(project: &Path) -> DagFileStore {
    create_dag_file_store(
        &config_with(
            project,
            DagSettingsOverrides {
                retention_days: Some(7),
                ..DagSettingsOverrides::default()
            },
        ),
        DagStoreOptions::default(),
    )
    .expect("store opens")
}

#[test]
fn given_expired_terminal_artifacts_and_equally_old_live_artifacts_when_retention_runs_then_only_the_terminal_run_is_pruned() {
    // given
    let now = retention_now();
    let project = temp_project();
    let store = retention_store(project.path());
    let old = "2026-01-01T00:00:00.000Z";
    let task_id = "st_dead-owner";
    store
        .write_checkpoint(RUN_ID, &checkpoint(RUN_ID, "completed", Some(old), Some(task_id)))
        .expect("terminal checkpoint");
    store
        .write_checkpoint(
            OTHER_RUN_ID,
            &checkpoint(OTHER_RUN_ID, "running", Some(old), None),
        )
        .expect("live checkpoint");
    store.append_event(&event(1)).expect("terminal event");
    store
        .append_event(&DagRunEvent {
            run_id: OTHER_RUN_ID.to_string(),
            ..event(1)
        })
        .expect("live event");
    store
        .write_result(RUN_ID, "node-a", "terminal result")
        .expect("terminal result");
    store
        .write_result(OTHER_RUN_ID, "node-a", "live result")
        .expect("live result");
    let terminal_key = store
        .write_key(&DagKeyRecord {
            schema_version: SchemaVersion1,
            parent_session_id: "parent-session".to_string(),
            run_key: "key-run-1".to_string(),
            run_id: RUN_ID.to_string(),
            definition_fingerprint: None,
        })
        .expect("terminal key");
    let live_key = store
        .write_key(&DagKeyRecord {
            schema_version: SchemaVersion1,
            parent_session_id: "parent-session".to_string(),
            run_key: "key-run-2".to_string(),
            run_id: OTHER_RUN_ID.to_string(),
            definition_fingerprint: None,
        })
        .expect("live key");
    let holder = json!({ "hostPid": 1, "runId": RUN_ID }).to_string();
    fs::write(store.paths.run_lock(RUN_ID), &holder).expect("run lock");
    fs::write(store.paths.key_lock("parent-session", "key-run-1"), &holder).expect("key lock");
    fs::write(store.paths.task_owner_lock(task_id), &holder).expect("task owner lock");

    // when
    let pruned = store.prune_expired(Some(now)).expect("prune");

    // then
    assert_eq!(pruned, vec![RUN_ID.to_string()]);
    assert!(!store.paths.run(RUN_ID).exists());
    assert!(!store.paths.event(RUN_ID).exists());
    assert!(!store.paths.results.join(RUN_ID).exists());
    assert!(!terminal_key.exists());
    assert!(!store.paths.run_lock(RUN_ID).exists());
    assert!(!store.paths.key_lock("parent-session", "key-run-1").exists());
    assert!(!store.paths.task_owner_lock(task_id).exists());
    assert!(store.paths.run(OTHER_RUN_ID).exists());
    assert!(store.paths.event(OTHER_RUN_ID).exists());
    assert!(store.paths.results.join(OTHER_RUN_ID).exists());
    assert!(live_key.exists());
}

#[test]
fn given_expired_terminal_and_equally_old_live_skill_sidecars_when_retention_runs_then_only_the_terminal_sidecar_is_pruned() {
    // given
    let now = retention_now();
    let old = "2026-01-01T00:00:00.000Z";
    let project = temp_project();
    let store = retention_store(project.path());
    store
        .write_checkpoint(RUN_ID, &checkpoint(RUN_ID, "completed", Some(old), None))
        .expect("terminal checkpoint");
    store
        .write_checkpoint(
            OTHER_RUN_ID,
            &checkpoint(OTHER_RUN_ID, "running", Some(old), None),
        )
        .expect("live checkpoint");
    let skills_directory = store.paths.root.join("skills");
    let terminal_skills = skills_directory.join(format!("{RUN_ID}.json"));
    let live_skills = skills_directory.join(format!("{OTHER_RUN_ID}.json"));
    fs::create_dir_all(&skills_directory).expect("skills dir");
    fs::write(&terminal_skills, json!({ "runId": RUN_ID }).to_string()).expect("terminal sidecar");
    fs::write(&live_skills, json!({ "runId": OTHER_RUN_ID }).to_string()).expect("live sidecar");

    // when
    store.prune_expired(Some(now)).expect("prune");

    // then
    assert!(!terminal_skills.exists());
    assert!(live_skills.exists());
}
