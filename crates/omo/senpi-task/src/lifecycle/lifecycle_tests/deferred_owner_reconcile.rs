//! `lifecycle/deferred-owner-reconcile.test.ts`: a child deferred to a dying foreign owner.
//!
//! The TS `wait` promises become channel gates: each retry wait reports its index on `entered` and
//! blocks until the test releases that index (or drops every gate at cleanup).

use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use super::*;
use crate::lifecycle::types::SuspendInput;
use crate::lifecycle::{
    LifecycleDeps, NoHostEndpoint, ReconcileOutcomeKind, TaskLifecycle, create_task_lifecycle,
};
use crate::state::{TaskStatus, TaskTransition};

const DEADLINE: Duration = Duration::from_secs(10);
const TASK_ID: &str = "st_97150001";
const OWNER_PID: i64 = 7101;
const CHILD_PID: i64 = 7102;
const HOST_PID: i64 = 7201;

type ClaimHook = Box<dyn FnOnce(&TaskRecordStore) + Send>;

/// The TS fixture's `{ ...store, mutate, appendEvent }`: a one-shot hook before the next mutate,
/// and a channel that fires when the task's `destroyed` event is appended.
struct ObservedStore {
    inner: Arc<TaskRecordStore>,
    before_claim: Mutex<Option<ClaimHook>>,
    destroyed: Sender<()>,
}

impl LifecycleStore for ObservedStore {
    fn state_dir(&self) -> &std::path::Path {
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
        let hook = self
            .before_claim
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(hook) = hook {
            hook(&self.inner);
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
        LifecycleStore::append_event(self.inner.as_ref(), task_id, event)?;
        if task_id == TASK_ID && event.event_type == "destroyed" {
            let _ = self.destroyed.send(());
        }
        Ok(())
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

/// Every retry wait, in call order; a wait returns when its gate is released or dropped.
#[derive(Default)]
pub(super) struct WaitGates {
    releases: Mutex<Gates>,
}

/// `closed` is set by cleanup: a wait that starts after it returns at once instead of parking.
#[derive(Default)]
struct Gates {
    pending: Vec<Option<Sender<()>>>,
    closed: bool,
}

impl WaitGates {
    pub(super) fn wait_fn(
        self: &Arc<Self>,
        entered: Sender<usize>,
    ) -> crate::lifecycle::context::RetryWaitFn {
        let gates = Arc::clone(self);
        let entered = Mutex::new(entered);
        Arc::new(move |_ms: u64| {
            let (release, gate) = channel::<()>();
            let index = {
                let mut releases = gates
                    .releases
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                if releases.closed {
                    return;
                }
                releases.pending.push(Some(release));
                releases.pending.len() - 1
            };
            let sender = entered
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone();
            let _ = sender.send(index);
            let _ = gate.recv();
        })
    }

    pub(super) fn release(&self, index: usize) {
        let release = self
            .releases
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pending
            .get_mut(index)
            .and_then(Option::take);
        if let Some(release) = release {
            let _ = release.send(());
        }
    }

    pub(super) fn len(&self) -> usize {
        self.releases
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pending
            .len()
    }

    pub(super) fn release_all(&self) {
        let mut releases = self.releases.lock().unwrap_or_else(PoisonError::into_inner);
        releases.closed = true;
        releases.pending.clear();
    }
}

struct Fixture {
    temp: TempStore,
    store: Arc<ObservedStore>,
    signaller: Arc<FakeSignaller>,
    gates: Arc<WaitGates>,
    entered: Receiver<usize>,
    destroyed: Receiver<()>,
    lifecycle: TaskLifecycle,
}

impl Fixture {
    fn new(startup_backoff_ms: Vec<u64>) -> Self {
        let temp = temp_store();
        seed_record(
            &temp.store,
            Seed {
                task_id: TASK_ID,
                parent_session_id: Some("old-parent"),
                status: Some(TaskStatus::Pending),
                execution_mode: Some("process"),
                host_pid: Some(OWNER_PID),
                pid: Some(CHILD_PID),
                ..Seed::default()
            },
        );
        temp.store
            .transition(
                TASK_ID,
                &TaskTransition::Start {
                    timestamp: crate::shared::iso_from_ms(now_ms()),
                    pid: None,
                    child_session_id: None,
                },
            )
            .expect("start");
        let (destroyed_tx, destroyed) = channel();
        let store = Arc::new(ObservedStore {
            inner: Arc::clone(&temp.store),
            before_claim: Mutex::new(None),
            destroyed: destroyed_tx,
        });
        let signaller = FakeSignaller::alive([OWNER_PID]);
        let gates = Arc::new(WaitGates::default());
        let (entered_tx, entered) = channel();
        let mut deps = LifecycleDeps::new(
            store.clone(),
            Arc::new(FakeRegistry::default()),
            settings(json!({ "reattach_on_reconcile": false })),
        );
        deps.host_pid = Some(HOST_PID);
        deps.host_endpoint = Some(Arc::new(NoHostEndpoint));
        deps.signaller = Some(signaller.clone());
        deps.orphan_kill_delay_ms = Some(0);
        // The 5 s live-parent scan is unfenced; it must not race the gated passes below.
        deps.live_parent_recovery =
            Some(crate::lifecycle::live_parent_recovery::RecoverySchedule::Disabled);
        deps.host_retry = Some(crate::lifecycle::HostSessionRetryPolicy {
            daemon_loss_backoff_ms: startup_backoff_ms,
            deferred_retry_backoff_ms: vec![1, 2],
            ..crate::lifecycle::default_host_session_retry_policy()
        });
        deps.host_retry_wait = Some(gates.wait_fn(entered_tx));
        Self {
            temp,
            store,
            signaller,
            gates,
            entered,
            destroyed,
            lifecycle: create_task_lifecycle(deps),
        }
    }

    fn record(&self) -> TaskRecord {
        self.temp
            .store
            .load(TASK_ID)
            .expect("load")
            .expect("record")
    }

    fn events(&self) -> Vec<String> {
        read_events(&self.temp.store, TASK_ID)
    }

    fn entered(&self, index: usize) {
        assert_eq!(
            self.entered.recv_timeout(DEADLINE),
            Ok(index),
            "retry wait {index} entered"
        );
    }

    fn destroyed(&self) {
        self.destroyed
            .recv_timeout(DEADLINE)
            .expect("reconcile destroyed the task");
    }

    fn settled(&self) {
        assert!(
            self.lifecycle
                .context()
                .deferred_owner_retries
                .wait_settled(TASK_ID, DEADLINE),
            "the deferred owner retry settled"
        );
    }

    fn before_claim(&self, hook: ClaimHook) {
        *self
            .store
            .before_claim
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(hook);
    }

    fn reconcile(&self, session: Option<&str>) -> Vec<crate::lifecycle::ReconcileOutcome> {
        self.lifecycle
            .reconcile_on_session_start(session)
            .expect("reconcile")
            .outcomes
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.lifecycle.dispose();
        self.gates.release_all();
        self.lifecycle
            .context()
            .deferred_owner_retries
            .wait_settled(TASK_ID, DEADLINE);
    }
}

#[test]
fn startup_observes_dead_child_recovery_before_a_one_response_parent_can_exit() {
    // Given a dead child whose owner is still visible during startup.
    let f = Fixture::new(vec![1, 2]);
    let lifecycle = f.lifecycle.clone();
    let startup = std::thread::spawn(move || {
        lifecycle
            .reconcile_on_session_start(Some("new-parent"))
            .expect("reconcile")
            .outcomes
    });
    f.entered(0);
    // When owner death is confirmed during the bounded startup retry.
    f.signaller.set_alive(OWNER_PID, false);
    f.gates.release(0);
    let startup = startup.join().expect("startup thread");
    // Then the startup caller cannot return with only the old deferred outcome: the task was
    // already destroyed when it returned.
    assert_eq!(f.destroyed.try_recv(), Ok(()));
    assert_eq!(startup[0].kind, ReconcileOutcomeKind::Lost);
    assert_eq!(f.record().status, TaskStatus::Lost);
}

fn tree_killed_child_reaches_lost(session: Option<&str>) {
    // Given the child is dead but the tree killer has not finished releasing its owner.
    let f = Fixture::new(Vec::new());
    let initial = f.reconcile(session);
    let expected = if session.is_none() {
        ReconcileOutcomeKind::ForeignLiveOwner
    } else {
        ReconcileOutcomeKind::Deferred
    };
    assert_eq!(initial[0].kind, expected);
    assert_eq!(f.record().status, TaskStatus::Running);
    f.entered(0);
    // When the first retry still sees the owner; the next sees confirmed death.
    f.gates.release(0);
    f.entered(1);
    assert_eq!(f.record().host_pid, Some(OWNER_PID));
    assert_eq!(f.events(), vec!["transition_applied"]);
    f.signaller.set_alive(OWNER_PID, false);
    f.gates.release(1);
    f.destroyed();
    // Then no second session_start is needed and no dead process is signalled.
    assert_eq!(f.record().status, TaskStatus::Lost);
    assert!(f.events().contains(&"reconcile_lost".to_string()));
    assert!(f.signaller.signals().is_empty());
}

#[test]
fn a_tree_killed_child_reaches_lost_after_its_still_visible_owner_exits_global() {
    tree_killed_child_reaches_lost(None);
}

#[test]
fn a_tree_killed_child_reaches_lost_after_its_still_visible_owner_exits_new_parent() {
    tree_killed_child_reaches_lost(Some("new-parent"));
}

#[test]
fn a_live_sibling_owner_is_never_lost_when_the_retry_budget_ends() {
    // Given a live owner throughout both attempts.
    let f = Fixture::new(Vec::new());
    f.reconcile(Some("new-parent"));
    f.entered(0);
    // When every bounded attempt sees the live owner.
    f.gates.release(0);
    f.entered(1);
    f.gates.release(1);
    f.settled();
    // A direct pass also observes the unchanged owner; it cannot steal its task.
    f.reconcile(Some("new-parent"));
    // Then the record and process remain owned by the sibling.
    assert_eq!(f.record().status, TaskStatus::Running);
    assert_eq!(f.record().host_pid, Some(OWNER_PID));
    assert_eq!(f.events(), vec!["transition_applied"]);
    assert!(f.signaller.signals().is_empty());
}

fn deferred_retry_cannot_reclaim_after(stop: &dyn Fn(&TaskLifecycle)) {
    // Given a deferred retry whose owning session is about to stop.
    let f = Fixture::new(Vec::new());
    f.reconcile(Some("new-parent"));
    f.entered(0);
    // When the observer stops before owner death is released to the retry.
    stop(&f.lifecycle);
    f.signaller.set_alive(OWNER_PID, false);
    f.gates.release(0);
    f.settled();
    // Then the stopped observer makes no record mutation.
    assert_eq!(f.gates.len(), 1);
    assert_eq!(f.record().status, TaskStatus::Running);
    assert_eq!(f.events(), vec!["transition_applied"]);
}

#[test]
fn a_deferred_retry_cannot_reclaim_after_dispose() {
    deferred_retry_cannot_reclaim_after(&TaskLifecycle::dispose);
}

#[test]
fn a_deferred_retry_cannot_reclaim_after_shutdown() {
    deferred_retry_cannot_reclaim_after(&|lifecycle| {
        lifecycle
            .suspend_on_session_shutdown(&SuspendInput {
                parent_session_id: "new-parent".to_string(),
                own_task_id: None,
                reason: "quit".to_string(),
            })
            .expect("shutdown");
    });
}

#[test]
fn an_aborted_pass_does_not_stop_its_background_owner_retry() {
    // Given a background owner retry spawned by a pass the caller can abort.
    let f = Fixture::new(Vec::new());
    let signal = crate::manager::AbortSignal::default();
    f.lifecycle
        .reconcile_on_session_start_with_signal(Some("new-parent"), Some(&signal))
        .expect("reconcile");
    f.entered(0);
    // When only the pass fence goes stale (no dispose, no shutdown of the session).
    signal.abort();
    f.signaller.set_alive(OWNER_PID, false);
    f.gates.release(0);
    // Then the retry runs on the base context, as upstream create.ts does, and still reclaims.
    f.destroyed();
    assert_eq!(f.record().status, TaskStatus::Lost);
    assert!(f.events().contains(&"reconcile_lost".to_string()));
}

#[test]
fn an_abort_during_the_startup_dead_child_retry_stops_it() {
    // Given a dead child whose startup retry is waiting inside an abortable pass.
    let f = Fixture::new(vec![1]);
    let signal = crate::manager::AbortSignal::default();
    let lifecycle = f.lifecycle.clone();
    let pass_signal = signal.clone();
    let startup = std::thread::spawn(move || {
        lifecycle
            .reconcile_on_session_start_with_signal(Some("new-parent"), Some(&pass_signal))
            .expect("reconcile")
            .outcomes
    });
    f.entered(0);
    // When the caller aborts during that wait and owner death is then visible.
    signal.abort();
    f.signaller.set_alive(OWNER_PID, false);
    f.gates.release(0);
    let outcomes = startup.join().expect("startup thread");
    // Then the fenced startup retry stops without a mutation and no background retry starts.
    assert!(outcomes.is_empty());
    assert_eq!(f.gates.len(), 1);
    assert_eq!(f.record().status, TaskStatus::Running);
    assert_eq!(f.events(), vec!["transition_applied"]);
}

#[test]
fn an_owner_alive_again_at_the_locked_claim_is_not_stolen() {
    // Given a dead-owner probe followed by a live PID before the claim.
    let f = Fixture::new(Vec::new());
    f.reconcile(Some("new-parent"));
    f.entered(0);
    f.signaller.set_alive(OWNER_PID, false);
    let signaller = f.signaller.clone();
    f.before_claim(Box::new(move |_: &TaskRecordStore| {
        signaller.set_alive(OWNER_PID, true);
    }));
    // When the retry attempts to claim, the locked liveness check refuses.
    f.gates.release(0);
    f.entered(1);
    assert_eq!(f.record().status, TaskStatus::Running);
    assert_eq!(f.record().host_pid, Some(OWNER_PID));
    assert_eq!(f.events(), vec!["transition_applied"]);
    // Then a subsequent confirmed death can still settle the deferred task.
    f.signaller.set_alive(OWNER_PID, false);
    f.gates.release(1);
    f.destroyed();
    assert_eq!(f.record().status, TaskStatus::Lost);
}

#[test]
fn a_genuine_error_written_at_the_claim_boundary_remains_error() {
    // Given the old manager's independent failure racing the retry's stale running snapshot.
    let f = Fixture::new(Vec::new());
    f.reconcile(Some("new-parent"));
    f.entered(0);
    f.signaller.set_alive(OWNER_PID, false);
    f.before_claim(Box::new(|store: &TaskRecordStore| {
        store
            .transition(
                TASK_ID,
                &TaskTransition::Fail {
                    timestamp: "2030-01-01T00:00:00.000Z".to_string(),
                    error_message: "independent crash".to_string(),
                    killed: false,
                    run_stats: None,
                },
            )
            .expect("fail");
    }));
    // When the locked claim re-reads the now-terminal record, its old snapshot loses.
    f.gates.release(0);
    f.entered(1);
    f.gates.release(1);
    f.settled();
    // Then neither the retry nor reconciliation relabels the failure.
    let record = f.record();
    assert_eq!(record.status, TaskStatus::Error);
    assert_eq!(record.error_message.as_deref(), Some("independent crash"));
    assert!(!f.events().contains(&"reconcile_lost".to_string()));
}

#[test]
fn a_live_replacement_owner_keeps_its_claim() {
    // Given a new owner claiming while the retry is deferred.
    let f = Fixture::new(Vec::new());
    f.reconcile(Some("new-parent"));
    f.entered(0);
    f.signaller.set_alive(OWNER_PID, false);
    f.signaller.set_alive(7301, true);
    f.temp
        .store
        .mutate(TASK_ID, |record| {
            let mut next = record.clone();
            next.host_pid = Some(7301);
            next.residency_claim = Some("new-claim".to_string());
            next
        })
        .expect("install replacement owner");
    // When the stale retry wakes, ownership no longer matches its observed claim.
    f.gates.release(0);
    f.settled();
    // Then no process or record belonging to the replacement is touched.
    assert_eq!(f.record().host_pid, Some(7301));
    assert_eq!(f.record().status, TaskStatus::Running);
    assert!(f.signaller.signals().is_empty());
}
