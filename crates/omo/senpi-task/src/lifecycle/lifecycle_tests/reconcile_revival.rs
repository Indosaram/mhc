//! `lifecycle/reconcile-revival.test.ts` (+ `__fixtures__/reconcile-race-worker.ts`), the
//! injected-port cases. The one case that drives a real `createTaskManager` (pending child with
//! durable steering) lives with the manager tests.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, Mutex, PoisonError};

use serde_json::{Value, json};

use super::*;
use crate::host::HostError;
use crate::lifecycle::admission_lease::{
    AcquireAdmissionLeaseResult, AdmissionLeaseTimingOverrides, acquire_session_admission_lease,
};
use crate::lifecycle::port::{
    ReattachResult, ResidentKind, RespawnDisposition, RespawnFailureCode, RespawnResult,
};
use crate::lifecycle::reconcile::child_session_dir;
use crate::lifecycle::residency::BatchAdmissionOptions;
use crate::lifecycle::{
    LifecycleDeps, ReconcileOutcome, ReconcileOutcomeKind, TaskLifecycle, create_task_lifecycle,
};
use crate::manager::child_handle::{ManagedChildHandle, ManagedChildListener, Unsubscribe};
use crate::runners::RunnerOutcome;
use crate::state::{ResidencyState::*, SpawnSpecV1, TaskSpawnSpec, TaskStatus::*};

const PARENT: &str = "session-resumed";
const HOST_PID: i64 = 2222;

type Launches = Arc<Mutex<Vec<(String, Option<PathBuf>)>>>;
type MutateFailure = Box<dyn FnMut(&str) -> bool + Send>;
type RespawnHook = Box<dyn Fn(&TaskRecord, &FakeSignaller) -> RespawnResult + Send + Sync>;

/// A managed handle whose turn never settles (`waitForOutcome: () => new Promise(() => {})`).
struct IdleManagedHandle {
    task_id: String,
}

impl ManagedChildHandle for IdleManagedHandle {
    fn task_id(&self) -> &str {
        &self.task_id
    }
    fn session_id(&self) -> Option<String> {
        Some(format!("session:{}", self.task_id))
    }
    fn pid(&self) -> Option<i64> {
        None
    }
    fn steer(&self, _text: &str) -> Result<(), HostError> {
        Ok(())
    }
    fn follow_up(&self, _text: &str) -> Result<(), HostError> {
        Ok(())
    }
    fn abort(&self) -> Result<(), HostError> {
        Ok(())
    }
    fn subscribe(&self, _listener: ManagedChildListener) -> Unsubscribe {
        Box::new(|| ())
    }
    fn wait_for_outcome(&self) -> RunnerOutcome {
        panic!("revival fakes never settle a turn; reconcile must not wait on one");
    }
    fn last_assistant_text(&self) -> Option<String> {
        None
    }
    fn dispose(&self) -> Result<(), HostError> {
        Ok(())
    }
}

fn success(task_id: &str) -> RespawnResult {
    RespawnResult::Ok(Arc::new(IdleManagedHandle {
        task_id: task_id.to_string(),
    }))
}

fn model_unavailable() -> RespawnResult {
    RespawnResult::Failed {
        disposition: RespawnDisposition::Retryable,
        code: RespawnFailureCode::ModelUnavailable,
        reason: "model registry is cold".to_string(),
    }
}

fn with_v1(store: &TaskRecordStore, record: TaskRecord) -> TaskRecord {
    let updated = TaskRecord {
        spawn_spec: Some(TaskSpawnSpec::V1(SpawnSpecV1 {
            cwd: "/tmp/project".to_string(),
            prompt: format!("prompt:{}", record.task_id),
            instructions: None,
            member_scoped_tool_names: None,
        })),
        ..record
    };
    store.replace(&updated).expect("replace");
    updated
}

fn persist_session(store: &TaskRecordStore, task_id: &str) -> PathBuf {
    let directory = child_session_dir(store.state_dir(), task_id);
    std::fs::create_dir_all(&directory).expect("session dir");
    let path = directory.join("resume.jsonl");
    std::fs::write(&path, "{}\n").expect("transcript");
    let mtime = std::time::UNIX_EPOCH + std::time::Duration::from_millis(2_000);
    std::fs::File::options()
        .write(true)
        .open(&path)
        .and_then(|file| file.set_modified(mtime))
        .expect("set mtime");
    path
}

fn seed(
    store: &TaskRecordStore,
    task_id: &str,
    status: TaskStatus,
    residency: ResidencyState,
    mode: &str,
) -> TaskRecord {
    seed_record(
        store,
        Seed {
            task_id,
            parent_session_id: Some(PARENT),
            status: Some(status),
            residency_state: Some(residency),
            execution_mode: Some(mode),
            ..Seed::default()
        },
    )
}

#[derive(Default)]
struct HarnessOptions {
    store: Option<Arc<dyn LifecycleStore>>,
    config: Option<Value>,
    alive: Vec<i64>,
    on_respawn: Option<RespawnHook>,
}

struct Harness {
    lifecycle: TaskLifecycle,
    launches: Launches,
    signaller: Arc<FakeSignaller>,
}

impl Harness {
    fn launched_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.launches().into_iter().map(|(id, _)| id).collect();
        ids.sort();
        ids
    }
    fn launches(&self) -> Vec<(String, Option<PathBuf>)> {
        self.launches
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
    fn reconcile(&self) -> Vec<ReconcileOutcome> {
        self.lifecycle
            .reconcile_on_session_start(Some(PARENT))
            .expect("reconcile")
            .outcomes
    }
}

fn harness(temp: &TempStore, options: HarnessOptions) -> Harness {
    let store: Arc<dyn LifecycleStore> = options.store.unwrap_or_else(|| temp.store.clone());
    let registry = Arc::new(FakeRegistry::default());
    let signaller = Arc::new(FakeSignaller {
        dies_on_term: true,
        ..FakeSignaller::with_alive(options.alive)
    });
    let launches: Launches = Arc::default();
    let mut deps = LifecycleDeps::new(
        store,
        registry.clone(),
        options.config.map_or_else(default_settings, settings),
    );
    deps.host_pid = Some(HOST_PID);
    deps.now = Some(Arc::new(|| 9_000_000));
    deps.signaller = Some(signaller.clone());
    deps.orphan_kill_delay_ms = Some(0);
    let respawn_launches = Arc::clone(&launches);
    let respawn_signaller = Arc::clone(&signaller);
    let on_respawn = options.on_respawn;
    deps.respawn = Some(Arc::new(
        move |record: &TaskRecord, session: Option<&Path>| {
            respawn_launches
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push((record.task_id.clone(), session.map(Path::to_path_buf)));
            on_respawn.as_ref().map_or_else(
                || success(&record.task_id),
                |hook| hook(record, &respawn_signaller),
            )
        },
    ));
    deps.reattach = Some(Arc::new(move |record: &TaskRecord, _handle| {
        let kind = if record.execution_mode == "process" {
            ResidentKind::Rpc
        } else {
            ResidentKind::InProcess
        };
        registry.add(fake_handle(
            &record.task_id,
            kind,
            &call_log(),
            HandleOptions {
                pid: record.pid,
                ..HandleOptions::default()
            },
        ));
        ReattachResult::Ok
    }));
    Harness {
        lifecycle: create_task_lifecycle(deps),
        launches,
        signaller,
    }
}

fn outcome<'a>(outcomes: &'a [ReconcileOutcome], task_id: &str) -> &'a ReconcileOutcome {
    outcomes
        .iter()
        .find(|outcome| outcome.task_id == task_id)
        .unwrap_or_else(|| panic!("no outcome for {task_id}: {outcomes:?}"))
}

fn deferred(task_id: &str, reason: &str) -> ReconcileOutcome {
    ReconcileOutcome::new(task_id, ReconcileOutcomeKind::Deferred, Some(reason))
}

/// `{ ...backing, mutate }`: fails selected `mutate` calls, delegates everything else.
struct MutateHookStore {
    inner: Arc<TaskRecordStore>,
    fail: Mutex<MutateFailure>,
}

impl MutateHookStore {
    fn new(
        inner: &Arc<TaskRecordStore>,
        fail: impl FnMut(&str) -> bool + Send + 'static,
    ) -> Arc<Self> {
        Arc::new(Self {
            inner: Arc::clone(inner),
            fail: Mutex::new(Box::new(fail)),
        })
    }
}

impl LifecycleStore for MutateHookStore {
    fn state_dir(&self) -> &Path {
        self.inner.state_dir()
    }
    fn load(&self, task_id: &str) -> Result<Option<TaskRecord>, StoreError> {
        self.inner.load(task_id)
    }
    fn list(&self) -> Result<ListTaskRecordsResult, StoreError> {
        self.inner.list()
    }
    fn mutate(
        &self,
        task_id: &str,
        mutation: &mut RecordMutation<'_>,
    ) -> Result<Option<TaskRecord>, StoreError> {
        if (self.fail.lock().unwrap_or_else(PoisonError::into_inner))(task_id) {
            return Err(StoreError::Io(std::io::Error::other(
                "record lock contended",
            )));
        }
        LifecycleStore::mutate(self.inner.as_ref(), task_id, mutation)
    }
    fn replace(&self, record: &TaskRecord) -> Result<(), StoreError> {
        self.inner.replace(record)
    }
    fn transition(
        &self,
        task_id: &str,
        transition: &TaskTransition,
    ) -> Result<TaskTransitionResult, StoreError> {
        self.inner.transition(task_id, transition)
    }
    fn append_event(&self, task_id: &str, event: &PersistedTaskEvent) -> Result<(), StoreError> {
        LifecycleStore::append_event(self.inner.as_ref(), task_id, event)
    }
    fn tombstone_if_expired(
        &self,
        task_id: &str,
        should_retain: &mut RetainPredicate<'_>,
    ) -> Result<TombstoneResult, StoreError> {
        LifecycleStore::tombstone_if_expired(self.inner.as_ref(), task_id, should_retain)
    }
    fn complete_expunge(&self, task_id: &str) -> Result<(), StoreError> {
        self.inner.complete_expunge(task_id)
    }
    fn list_expunging(&self) -> Result<Vec<String>, StoreError> {
        self.inner.list_expunging()
    }
}

/// Fails the `nth` (0-based) `mutate` of `task_id` only.
fn fail_nth_mutation(task_id: &str, nth: usize) -> impl FnMut(&str) -> bool + Send + 'static {
    let task_id = task_id.to_string();
    let mut seen = 0;
    move |id| {
        if id != task_id {
            return false;
        }
        seen += 1;
        seen - 1 == nth
    }
}

#[test]
fn suspended_in_process_child_resumes_through_the_port() {
    let temp = temp_store();
    let h = harness(&temp, HarnessOptions::default());
    let record = with_v1(
        &temp.store,
        seed(
            &temp.store,
            "st_10000001",
            Running,
            PersistedOnly,
            "in-process",
        ),
    );
    let session = persist_session(&temp.store, &record.task_id);
    let outcomes = h.reconcile();
    assert!(outcomes.contains(&ReconcileOutcome::new(
        &record.task_id,
        ReconcileOutcomeKind::Resumed,
        Some("respawned and reattached")
    )));
    assert_eq!(h.launches(), vec![(record.task_id.clone(), Some(session))]);
    let stored = temp
        .store
        .load(&record.task_id)
        .expect("load")
        .expect("record");
    assert_eq!(stored.residency_state, Resident);
    assert_eq!(stored.host_pid, Some(HOST_PID));
}

#[test]
fn legacy_crash_orphan_of_another_session_still_reattaches_globally() {
    let temp = temp_store();
    let h = harness(&temp, HarnessOptions::default());
    let other = with_v1(
        &temp.store,
        seed_record(
            &temp.store,
            Seed {
                task_id: "st_10000015",
                parent_session_id: Some("session-other"),
                status: Some(Running),
                residency_state: Some(Resident),
                execution_mode: Some("process"),
                pid: Some(7400),
                host_pid: Some(9999),
                ..Seed::default()
            },
        ),
    );
    let session = persist_session(&temp.store, &other.task_id);
    let outcomes = h.reconcile();
    assert_eq!(
        outcome(&outcomes, &other.task_id).kind,
        ReconcileOutcomeKind::Resumed
    );
    assert!(
        h.launches()
            .contains(&(other.task_id.clone(), Some(session)))
    );
}

#[test]
fn terminal_record_owned_by_a_live_sibling_is_untouched() {
    let temp = temp_store();
    let record = seed_record(
        &temp.store,
        Seed {
            task_id: "st_10000016",
            parent_session_id: Some(PARENT),
            status: Some(Completed),
            residency_state: Some(Resident),
            execution_mode: Some("process"),
            pid: Some(7401),
            host_pid: Some(3333),
            ..Seed::default()
        },
    );
    let h = harness(
        &temp,
        HarnessOptions {
            alive: vec![3333, 7401],
            ..HarnessOptions::default()
        },
    );
    let outcomes = h.reconcile();
    assert!(outcomes.contains(&deferred(&record.task_id, "foreign_live_owner")));
    assert_eq!(
        temp.store.load(&record.task_id).expect("load"),
        Some(record)
    );
    assert!(h.signaller.signals().is_empty());
}

#[test]
fn suspended_child_of_another_session_is_untouched() {
    let temp = temp_store();
    let h = harness(&temp, HarnessOptions::default());
    let other = with_v1(
        &temp.store,
        seed_record(
            &temp.store,
            Seed {
                task_id: "st_10000002",
                parent_session_id: Some("session-other"),
                status: Some(Running),
                residency_state: Some(PersistedOnly),
                execution_mode: Some("in-process"),
                ..Seed::default()
            },
        ),
    );
    h.reconcile();
    assert!(h.launches().is_empty());
    assert_eq!(temp.store.load(&other.task_id).expect("load"), Some(other));
}

/// `Promise.all([first.reconcile(), second.reconcile()])` in the TS test: both `list()` reads run
/// synchronously before either manager reaches its first `await` (the lease acquisition), so both
/// see the suspended child. Rust `reconcile` is synchronous end to end, so the two harnesses
/// rendezvous after their second `list()` (the scoped-revival candidate scan) to reproduce that
/// ordering instead of racing on which manager finishes first.
struct RendezvousStore {
    inner: Arc<TaskRecordStore>,
    gate: Arc<Barrier>,
    lists: AtomicUsize,
}

impl RendezvousStore {
    fn new(inner: &Arc<TaskRecordStore>, gate: &Arc<Barrier>) -> Self {
        Self {
            inner: Arc::clone(inner),
            gate: Arc::clone(gate),
            lists: AtomicUsize::new(0),
        }
    }
}

impl LifecycleStore for RendezvousStore {
    fn state_dir(&self) -> &Path {
        self.inner.state_dir()
    }
    fn load(&self, task_id: &str) -> Result<Option<TaskRecord>, StoreError> {
        self.inner.load(task_id)
    }
    fn list(&self) -> Result<ListTaskRecordsResult, StoreError> {
        let result = self.inner.list();
        if self.lists.fetch_add(1, Ordering::SeqCst) + 1 == 2 {
            self.gate.wait();
        }
        result
    }
    fn mutate(
        &self,
        task_id: &str,
        mutation: &mut RecordMutation<'_>,
    ) -> Result<Option<TaskRecord>, StoreError> {
        LifecycleStore::mutate(self.inner.as_ref(), task_id, mutation)
    }
    fn replace(&self, record: &TaskRecord) -> Result<(), StoreError> {
        self.inner.replace(record)
    }
    fn transition(
        &self,
        task_id: &str,
        transition: &TaskTransition,
    ) -> Result<TaskTransitionResult, StoreError> {
        self.inner.transition(task_id, transition)
    }
    fn append_event(&self, task_id: &str, event: &PersistedTaskEvent) -> Result<(), StoreError> {
        LifecycleStore::append_event(self.inner.as_ref(), task_id, event)
    }
    fn tombstone_if_expired(
        &self,
        task_id: &str,
        should_retain: &mut RetainPredicate<'_>,
    ) -> Result<TombstoneResult, StoreError> {
        LifecycleStore::tombstone_if_expired(self.inner.as_ref(), task_id, should_retain)
    }
    fn complete_expunge(&self, task_id: &str) -> Result<(), StoreError> {
        self.inner.complete_expunge(task_id)
    }
    fn list_expunging(&self) -> Result<Vec<String>, StoreError> {
        self.inner.list_expunging()
    }
}

#[test]
fn two_managers_racing_one_suspended_child_claim_once() {
    let temp = temp_store();
    let record = with_v1(
        &temp.store,
        seed(
            &temp.store,
            "st_10000003",
            Running,
            PersistedOnly,
            "in-process",
        ),
    );
    persist_session(&temp.store, &record.task_id);
    let gate = Arc::new(Barrier::new(2));
    let first = harness(
        &temp,
        HarnessOptions {
            store: Some(Arc::new(RendezvousStore::new(&temp.store, &gate))),
            ..HarnessOptions::default()
        },
    );
    let second = harness(
        &temp,
        HarnessOptions {
            store: Some(Arc::new(RendezvousStore::new(&temp.store, &gate))),
            ..HarnessOptions::default()
        },
    );
    let (a, b) = std::thread::scope(|scope| {
        let (first, second) = (&first, &second);
        let a = scope.spawn(move || first.reconcile());
        let b = scope.spawn(move || second.reconcile());
        (a.join().expect("first"), b.join().expect("second"))
    });
    assert_eq!(first.launches().len() + second.launches().len(), 1);
    let mut kinds: Vec<ReconcileOutcomeKind> =
        a.iter().chain(&b).map(|outcome| outcome.kind).collect();
    kinds.sort_by_key(|kind| format!("{kind:?}"));
    assert_eq!(
        kinds,
        vec![
            ReconcileOutcomeKind::Deferred,
            ReconcileOutcomeKind::Resumed
        ]
    );
}

#[test]
fn only_this_sessions_in_process_and_rpc_children_revive() {
    let temp = temp_store();
    let h = harness(&temp, HarnessOptions::default());
    let own_in_process = with_v1(
        &temp.store,
        seed(
            &temp.store,
            "st_10000017",
            Running,
            PersistedOnly,
            "in-process",
        ),
    );
    let own_rpc = with_v1(
        &temp.store,
        seed_record(
            &temp.store,
            Seed {
                task_id: "st_10000018",
                parent_session_id: Some(PARENT),
                status: Some(Running),
                residency_state: Some(RpcDetached),
                execution_mode: Some("process"),
                pid: Some(7402),
                ..Seed::default()
            },
        ),
    );
    with_v1(
        &temp.store,
        seed_record(
            &temp.store,
            Seed {
                task_id: "st_10000019",
                parent_session_id: Some("session-other"),
                status: Some(Running),
                residency_state: Some(PersistedOnly),
                execution_mode: Some("in-process"),
                ..Seed::default()
            },
        ),
    );
    persist_session(&temp.store, &own_in_process.task_id);
    persist_session(&temp.store, &own_rpc.task_id);
    let outcomes = h.reconcile();
    assert_eq!(
        h.launched_ids(),
        vec![own_in_process.task_id, own_rpc.task_id]
    );
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| outcome.kind == ReconcileOutcomeKind::Resumed)
            .count(),
        2
    );
    assert_eq!(residency(&temp.store, "st_10000019"), Some(PersistedOnly));
}

#[test]
fn contended_terminal_disposal_never_relaunches_and_defers() {
    let temp = temp_store();
    let record = with_v1(
        &temp.store,
        seed(
            &temp.store,
            "st_1000001a",
            Completed,
            PersistedOnly,
            "in-process",
        ),
    );
    let store = MutateHookStore::new(&temp.store, fail_nth_mutation(&record.task_id, 0));
    let h = harness(
        &temp,
        HarnessOptions {
            store: Some(store),
            ..HarnessOptions::default()
        },
    );
    let outcomes = h.reconcile();
    assert!(h.launches().is_empty());
    assert!(outcomes.contains(&deferred(&record.task_id, "lock_contended")));
    assert_eq!(residency(&temp.store, &record.task_id), Some(PersistedOnly));
}

#[test]
fn temporarily_unavailable_model_rolls_back_ownership_and_defers() {
    let temp = temp_store();
    let h = harness(
        &temp,
        HarnessOptions {
            on_respawn: Some(Box::new(|_, _| model_unavailable())),
            ..HarnessOptions::default()
        },
    );
    let record = with_v1(
        &temp.store,
        seed(
            &temp.store,
            "st_10000004",
            Running,
            PersistedOnly,
            "in-process",
        ),
    );
    persist_session(&temp.store, &record.task_id);
    let outcomes = h.reconcile();
    assert!(outcomes.contains(&deferred(&record.task_id, "model_unavailable")));
    let stored = temp
        .store
        .load(&record.task_id)
        .expect("load")
        .expect("record");
    assert_eq!(stored.residency_state, PersistedOnly);
    assert_eq!(stored.host_pid, None);
}

#[test]
fn failed_rollback_after_retryable_respawn_is_named_loudly() {
    let temp = temp_store();
    let record = with_v1(
        &temp.store,
        seed(
            &temp.store,
            "st_1000001b",
            Running,
            PersistedOnly,
            "in-process",
        ),
    );
    persist_session(&temp.store, &record.task_id);
    let store = MutateHookStore::new(&temp.store, fail_nth_mutation(&record.task_id, 1));
    let h = harness(
        &temp,
        HarnessOptions {
            store: Some(store),
            on_respawn: Some(Box::new(|_, _| model_unavailable())),
            ..HarnessOptions::default()
        },
    );
    let outcomes = h.reconcile();
    assert!(outcomes.contains(&deferred(&record.task_id, "rollback_failed")));
    let stored = temp
        .store
        .load(&record.task_id)
        .expect("load")
        .expect("record");
    assert_eq!(stored.residency_state, Resident);
    assert_eq!(stored.host_pid, Some(HOST_PID));
}

#[test]
fn one_throwing_record_lock_defers_only_that_record() {
    let temp = temp_store();
    let blocked = with_v1(
        &temp.store,
        seed(
            &temp.store,
            "st_10000005",
            Running,
            PersistedOnly,
            "in-process",
        ),
    );
    let healthy = with_v1(
        &temp.store,
        seed(
            &temp.store,
            "st_10000006",
            Running,
            PersistedOnly,
            "in-process",
        ),
    );
    persist_session(&temp.store, &blocked.task_id);
    persist_session(&temp.store, &healthy.task_id);
    let blocked_id = blocked.task_id.clone();
    let store = MutateHookStore::new(&temp.store, move |id| id == blocked_id);
    let h = harness(
        &temp,
        HarnessOptions {
            store: Some(store),
            ..HarnessOptions::default()
        },
    );
    let outcomes = h.reconcile();
    assert!(outcomes.contains(&deferred(&blocked.task_id, "lock_contended")));
    assert_eq!(
        outcome(&outcomes, &healthy.task_id).kind,
        ReconcileOutcomeKind::Resumed
    );
}

#[test]
fn cancelled_and_killed_suspended_records_are_never_revived() {
    let temp = temp_store();
    let h = harness(&temp, HarnessOptions::default());
    seed(
        &temp.store,
        "st_10000007",
        Cancelled,
        PersistedOnly,
        "in-process",
    );
    seed_record(
        &temp.store,
        Seed {
            task_id: "st_10000008",
            parent_session_id: Some(PARENT),
            status: Some(Error),
            killed: true,
            residency_state: Some(PersistedOnly),
            execution_mode: Some("in-process"),
            ..Seed::default()
        },
    );
    h.reconcile();
    assert!(h.launches().is_empty());
}

#[test]
fn legacy_records_without_transcript_or_v1_spec() {
    let temp = temp_store();
    let h = harness(&temp, HarnessOptions::default());
    let running = seed(
        &temp.store,
        "st_1000000a",
        Running,
        PersistedOnly,
        "in-process",
    );
    let terminal = seed(
        &temp.store,
        "st_1000000b",
        Completed,
        PersistedOnly,
        "in-process",
    );
    temp.store
        .replace(&TaskRecord {
            final_response: Some("durable result".to_string()),
            ..terminal.clone()
        })
        .expect("replace");
    h.reconcile();
    assert_eq!(
        temp.store
            .load(&running.task_id)
            .expect("load")
            .expect("record")
            .status,
        Lost
    );
    let stored = temp
        .store
        .load(&terminal.task_id)
        .expect("load")
        .expect("record");
    assert_eq!(stored.status, Completed);
    assert_eq!(stored.final_response.as_deref(), Some("durable result"));
    assert_eq!(stored.residency_state, Disposed);
}

#[test]
fn terminal_v1_record_without_transcript_is_disposed_without_rerun() {
    let temp = temp_store();
    let h = harness(&temp, HarnessOptions::default());
    let record = with_v1(
        &temp.store,
        seed(
            &temp.store,
            "st_1000000c",
            Completed,
            PersistedOnly,
            "in-process",
        ),
    );
    temp.store
        .replace(&TaskRecord {
            final_response: Some("finished".to_string()),
            ..record.clone()
        })
        .expect("replace");
    let outcomes = h.reconcile();
    assert!(h.launches().is_empty());
    let stored = temp
        .store
        .load(&record.task_id)
        .expect("load")
        .expect("record");
    assert_eq!(stored.final_response.as_deref(), Some("finished"));
    assert_eq!(stored.residency_state, Disposed);
    let reason = outcome(&outcomes, &record.task_id)
        .reason
        .clone()
        .unwrap_or_default();
    assert!(reason.contains("terminal without transcript"), "{reason}");
}

#[test]
fn live_orphaned_rpc_pid_is_proven_dead_before_replacement_spawn() {
    let temp = temp_store();
    let alive_at_respawn = Arc::new(Mutex::new(None::<bool>));
    let observed = Arc::clone(&alive_at_respawn);
    let h = harness(
        &temp,
        HarnessOptions {
            alive: vec![7331],
            on_respawn: Some(Box::new(move |record, signaller| {
                *observed.lock().unwrap_or_else(PoisonError::into_inner) = Some(
                    crate::lifecycle::port::ProcessSignaller::is_alive(signaller, 7331),
                );
                success(&record.task_id)
            })),
            ..HarnessOptions::default()
        },
    );
    let record = with_v1(
        &temp.store,
        seed_record(
            &temp.store,
            Seed {
                task_id: "st_1000000d",
                parent_session_id: Some(PARENT),
                status: Some(Running),
                residency_state: Some(Resident),
                execution_mode: Some("process"),
                pid: Some(7331),
                host_pid: Some(9999),
                ..Seed::default()
            },
        ),
    );
    persist_session(&temp.store, &record.task_id);
    h.reconcile();
    assert_eq!(h.signaller.signals(), vec![(7331, "SIGTERM")]);
    assert_eq!(
        *alive_at_respawn
            .lock()
            .unwrap_or_else(PoisonError::into_inner),
        Some(false)
    );
    assert_eq!(h.launches().len(), 1);
}

#[test]
fn killed_orphaned_resident_at_the_cap_is_disposed_and_frees_its_slot() {
    let temp = temp_store();
    let h = harness(
        &temp,
        HarnessOptions {
            config: Some(json!({ "residency_max_children": 1 })),
            ..HarnessOptions::default()
        },
    );
    seed_record(
        &temp.store,
        Seed {
            task_id: "st_1000000e",
            parent_session_id: Some(PARENT),
            status: Some(Error),
            killed: true,
            residency_state: Some(Resident),
            execution_mode: Some("in-process"),
            host_pid: Some(9999),
            ..Seed::default()
        },
    );
    let waiting = with_v1(
        &temp.store,
        seed(
            &temp.store,
            "st_1000000f",
            Running,
            PersistedOnly,
            "in-process",
        ),
    );
    persist_session(&temp.store, &waiting.task_id);
    h.reconcile();
    assert_eq!(residency(&temp.store, "st_1000000e"), Some(Disposed));
    assert_eq!(residency(&temp.store, &waiting.task_id), Some(Resident));
}

#[test]
fn same_process_orphan_bypasses_a_full_cap() {
    let temp = temp_store();
    let h = harness(
        &temp,
        HarnessOptions {
            config: Some(json!({ "residency_max_children": 1 })),
            ..HarnessOptions::default()
        },
    );
    let orphan = with_v1(
        &temp.store,
        seed_record(
            &temp.store,
            Seed {
                task_id: "st_10000010",
                parent_session_id: Some(PARENT),
                status: Some(Running),
                residency_state: Some(Resident),
                execution_mode: Some("in-process"),
                host_pid: Some(HOST_PID),
                ..Seed::default()
            },
        ),
    );
    persist_session(&temp.store, &orphan.task_id);
    let outcomes = h.reconcile();
    assert_eq!(
        outcome(&outcomes, &orphan.task_id).kind,
        ReconcileOutcomeKind::Resumed
    );
    assert_eq!(h.launches().len(), 1);
}

const WORKER_STATE_ENV: &str = "SENPI_TASK_REVIVAL_WORKER_STATE_DIR";
const WORKER_LOG_ENV: &str = "SENPI_TASK_REVIVAL_WORKER_LAUNCH_LOG";
const WORKER_TEST: &str = "lifecycle::lifecycle_tests::reconcile_revival::revival_race_worker";

fn read_byte() {
    let mut byte = [0_u8; 1];
    std::io::stdin()
        .read_exact(&mut byte)
        .expect("parent signal");
}

/// `__fixtures__/reconcile-race-worker.ts`; a no-op unless re-executed by the race test.
#[test]
fn revival_race_worker() {
    let (Ok(state_dir), Ok(launch_log)) = (
        std::env::var(WORKER_STATE_ENV),
        std::env::var(WORKER_LOG_ENV),
    ) else {
        return;
    };
    let backing = Arc::new(TaskRecordStore::new(&StateDirConfig {
        project_dir: PathBuf::from(&state_dir),
        task_state_dir: Some(PathBuf::from(&state_dir)),
    }));
    // Direct writes bypass libtest's output capture.
    let store = RacingStore::new(
        &backing,
        Box::new(|_| {
            let mut stdout = std::io::stdout();
            writeln!(stdout, "scanned").expect("write scanned");
            stdout.flush().expect("flush");
            read_byte();
        }),
    );
    let pid = i64::from(std::process::id());
    let mut deps = LifecycleDeps::new(store, Arc::new(FakeRegistry::default()), default_settings());
    deps.host_pid = Some(pid);
    deps.respawn = Some(Arc::new(move |record: &TaskRecord, _| {
        let mut log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&launch_log)
            .expect("launch log");
        writeln!(log, "{pid}:{}", record.task_id).expect("append launch");
        success(&record.task_id)
    }));
    deps.reattach = Some(Arc::new(|_, _| ReattachResult::Ok));
    let result = create_task_lifecycle(deps)
        .reconcile_on_session_start(Some(PARENT))
        .expect("reconcile");
    let outcomes: Vec<String> = result
        .outcomes
        .iter()
        .map(|outcome| {
            format!(
                "{}:{}",
                format!("{:?}", outcome.kind).to_lowercase(),
                outcome.reason.as_deref().unwrap_or("undefined")
            )
        })
        .collect();
    let mut stdout = std::io::stdout();
    writeln!(stdout, "{}", json!({ "outcomes": outcomes })).expect("write result");
    stdout.flush().expect("flush");
    // Stay alive (a live owner) until the parent has both results.
    read_byte();
}

#[test]
fn two_os_processes_race_one_dead_owner_resident() {
    let temp = temp_store();
    let orphan = with_v1(
        &temp.store,
        seed_record(
            &temp.store,
            Seed {
                task_id: "st_10000014",
                parent_session_id: Some(PARENT),
                status: Some(Running),
                residency_state: Some(Resident),
                execution_mode: Some("in-process"),
                host_pid: Some(2_147_483_647),
                updated_at: Some("1970-01-01T00:16:40.000Z".to_string()),
                ..Seed::default()
            },
        ),
    );
    persist_session(&temp.store, &orphan.task_id);
    let launch_log = temp.store.state_dir().join("race-launches.log");
    let exe = std::env::current_exe().expect("test binary");
    let mut children: Vec<_> = (0..2)
        .map(|_| {
            Command::new(&exe)
                .args([WORKER_TEST, "--exact", "--nocapture", "--test-threads=1"])
                .env(WORKER_STATE_ENV, temp.store.state_dir())
                .env(WORKER_LOG_ENV, &launch_log)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("spawn worker")
        })
        .collect();
    let mut readers: Vec<_> = children
        .iter_mut()
        .map(|child| BufReader::new(child.stdout.take().expect("stdout")))
        .collect();
    let mut stdins: Vec<_> = children
        .iter_mut()
        .map(|child| child.stdin.take().expect("stdin"))
        .collect();
    let read_until = |reader: &mut BufReader<std::process::ChildStdout>,
                      accept: &dyn Fn(&str) -> bool| {
        let mut line = String::new();
        loop {
            line.clear();
            assert_ne!(
                reader.read_line(&mut line).expect("read worker"),
                0,
                "worker stdout ended early"
            );
            if accept(line.trim_end()) {
                return line.trim_end().to_string();
            }
        }
    };
    // libtest prefixes the first output line with "test <name> ... ".
    for reader in &mut readers {
        read_until(reader, &|line| line.ends_with("scanned"));
    }
    for stdin in &mut stdins {
        stdin.write_all(b"g").expect("release scan barrier");
        stdin.flush().expect("flush");
    }
    let mut outcomes: Vec<String> = readers
        .iter_mut()
        .flat_map(|reader| {
            let line = read_until(reader, &|line| line.starts_with('{'));
            let parsed: Value = serde_json::from_str(&line).expect("worker json");
            parsed["outcomes"]
                .as_array()
                .expect("outcomes")
                .iter()
                .map(|entry| entry.as_str().expect("outcome").to_string())
                .collect::<Vec<_>>()
        })
        .collect();
    for mut stdin in stdins {
        stdin.write_all(b"x").expect("release exit");
    }
    for mut child in children {
        let status = child.wait().expect("worker exit");
        let mut stderr = String::new();
        if let Some(mut pipe) = child.stderr.take() {
            pipe.read_to_string(&mut stderr).expect("stderr");
        }
        assert!(status.success(), "{stderr}");
    }
    let launches = std::fs::read_to_string(&launch_log).expect("launch log");
    assert_eq!(launches.lines().filter(|line| !line.is_empty()).count(), 1);
    outcomes.sort();
    assert_eq!(
        outcomes,
        vec![
            "deferred:foreign_live_owner",
            "resumed:respawned and reattached"
        ]
    );
}

#[test]
fn disabled_reattach_or_resume_keeps_suspended_records_deferred() {
    let reattach_temp = temp_store();
    let disabled_reattach = harness(
        &reattach_temp,
        HarnessOptions {
            config: Some(json!({ "reattach_on_reconcile": false })),
            ..HarnessOptions::default()
        },
    );
    let first = with_v1(
        &reattach_temp.store,
        seed(
            &reattach_temp.store,
            "st_10000011",
            Running,
            PersistedOnly,
            "in-process",
        ),
    );
    let resume_temp = temp_store();
    let disabled_resume = harness(
        &resume_temp,
        HarnessOptions {
            config: Some(json!({ "resume_children": false })),
            ..HarnessOptions::default()
        },
    );
    let second = with_v1(
        &resume_temp.store,
        seed(
            &resume_temp.store,
            "st_10000012",
            Running,
            PersistedOnly,
            "in-process",
        ),
    );
    let reattach_outcomes = disabled_reattach.reconcile();
    disabled_resume.reconcile();
    assert!(reattach_outcomes.contains(&deferred(&first.task_id, "reattach_disabled")));
    assert_eq!(
        residency(&reattach_temp.store, &first.task_id),
        Some(PersistedOnly)
    );
    assert_eq!(
        residency(&resume_temp.store, &second.task_id),
        Some(PersistedOnly)
    );
    assert!(disabled_resume.launches().is_empty());
}

#[test]
fn ten_suspended_children_at_capacity_eight_defer_overflow_without_loss() {
    let temp = temp_store();
    let h = harness(
        &temp,
        HarnessOptions {
            config: Some(json!({ "residency_max_children": 8 })),
            ..HarnessOptions::default()
        },
    );
    for index in 0..10 {
        let id = format!("st_100001{index:02}");
        let record = with_v1(
            &temp.store,
            seed(&temp.store, &id, Running, PersistedOnly, "in-process"),
        );
        persist_session(&temp.store, &record.task_id);
    }
    let outcomes = h.reconcile();
    assert_eq!(h.launches().len(), 8);
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| outcome.kind == ReconcileOutcomeKind::Deferred
                && outcome.reason.as_deref() == Some("capacity"))
            .count(),
        2
    );
    assert_eq!(
        temp.store
            .list()
            .expect("list")
            .records
            .iter()
            .filter(|record| record.status == Lost)
            .count(),
        0
    );
}

#[test]
fn admission_lease_held_past_the_bounded_wait_defers_without_error() {
    let temp = temp_store();
    let record = with_v1(
        &temp.store,
        seed(
            &temp.store,
            "st_10000013",
            Running,
            PersistedOnly,
            "in-process",
        ),
    );
    let AcquireAdmissionLeaseResult::Acquired(held) = acquire_session_admission_lease(
        temp.store.state_dir(),
        PARENT,
        AdmissionLeaseTimingOverrides {
            renew_ms: Some(10),
            stale_ms: Some(100),
            acquire_timeout_ms: Some(20),
            retry_ms: Some(2),
        },
    )
    .expect("acquire") else {
        panic!("expected held admission lease");
    };
    let mut deps = LifecycleDeps::new(
        temp.store.clone(),
        Arc::new(FakeRegistry::default()),
        default_settings(),
    );
    deps.host_pid = Some(HOST_PID);
    deps.now = Some(Arc::new(|| 9_000_000));
    deps.reconcile_admission = Some(BatchAdmissionOptions {
        timing: AdmissionLeaseTimingOverrides {
            acquire_timeout_ms: Some(20),
            retry_ms: Some(2),
            ..AdmissionLeaseTimingOverrides::default()
        },
        ..BatchAdmissionOptions::default()
    });
    deps.respawn = Some(Arc::new(|fresh: &TaskRecord, _| success(&fresh.task_id)));
    deps.reattach = Some(Arc::new(|_, _| ReattachResult::Ok));
    let result = create_task_lifecycle(deps).reconcile_on_session_start(Some(PARENT));
    held.release();
    assert!(
        result
            .expect("reconcile")
            .outcomes
            .contains(&deferred(&record.task_id, "lock_contended"))
    );
}
