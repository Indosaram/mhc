//! Translated from `steering/engine.test.ts` and `steering/engine-one-shot.test.ts`.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};

use pretty_assertions::assert_eq;

use super::*;
use crate::agents::interaction_policy_for_agent;
use crate::host::HostError;
use crate::lifecycle::DestroyCause;
use crate::manager::{ManagedChildHandle, ManagedChildListener, Unsubscribe};
use crate::runners::RunnerOutcome;
use crate::state::{
    DeliverAs, TaskRecord, TaskRecordInput, TaskStatus, TaskTransition, create_task_record,
};
use crate::store::{StateDirConfig, TaskRecordDiagnostic, TaskRecordStore};

type Shared<T> = Arc<Mutex<T>>;

fn now_iso() -> String {
    crate::shared::iso_from_ms(chrono::Utc::now().timestamp_millis())
}

fn lock<T>(value: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    value.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Clone, Copy, Debug)]
enum Flavor {
    InProcess,
    Rpc,
}

const FLAVORS: [Flavor; 2] = [Flavor::InProcess, Flavor::Rpc];

#[derive(Default)]
struct FakeHandle {
    task_id: String,
    pid: Option<i64>,
    abort_rejects: bool,
    steer_calls: Shared<Vec<String>>,
    follow_up_calls: Shared<Vec<String>>,
    abort_calls: Shared<usize>,
    /// Ordered `steer:x` / `followUp:x` log (the TS orderTrackingHandle wrapper).
    order: Shared<Vec<String>>,
    last_text: Shared<Option<String>>,
}

impl FakeHandle {
    fn new(task_id: &str, flavor: Flavor) -> Arc<Self> {
        Arc::new(Self::build(task_id, flavor, false))
    }

    fn build(task_id: &str, flavor: Flavor, abort_rejects: bool) -> Self {
        Self {
            task_id: task_id.to_string(),
            pid: matches!(flavor, Flavor::Rpc).then_some(4321),
            abort_rejects,
            ..Self::default()
        }
    }

    fn steers(&self) -> Vec<String> {
        lock(&self.steer_calls).clone()
    }

    fn follow_ups(&self) -> Vec<String> {
        lock(&self.follow_up_calls).clone()
    }

    fn aborts(&self) -> usize {
        *lock(&self.abort_calls)
    }

    fn order(&self) -> Vec<String> {
        lock(&self.order).clone()
    }
}

impl ManagedChildHandle for FakeHandle {
    fn task_id(&self) -> &str {
        &self.task_id
    }
    fn session_id(&self) -> Option<String> {
        Some(format!("sess-{}", self.task_id))
    }
    fn pid(&self) -> Option<i64> {
        self.pid
    }
    fn steer(&self, text: &str) -> Result<(), HostError> {
        lock(&self.order).push(format!("steer:{text}"));
        lock(&self.steer_calls).push(text.to_string());
        Ok(())
    }
    fn follow_up(&self, text: &str) -> Result<(), HostError> {
        lock(&self.order).push(format!("followUp:{text}"));
        lock(&self.follow_up_calls).push(text.to_string());
        Ok(())
    }
    fn abort(&self) -> Result<(), HostError> {
        *lock(&self.abort_calls) += 1;
        if self.abort_rejects {
            return Err(HostError {
                message: "child already exited".to_string(),
            });
        }
        Ok(())
    }
    fn subscribe(&self, _listener: ManagedChildListener) -> Unsubscribe {
        Box::new(|| {})
    }
    fn wait_for_outcome(&self) -> RunnerOutcome {
        RunnerOutcome::Cancelled
    }
    fn last_assistant_text(&self) -> Option<String> {
        lock(&self.last_text).clone()
    }
    fn dispose(&self) -> Result<(), HostError> {
        Ok(())
    }
}

#[derive(Default)]
struct FakeDestruction {
    calls: Mutex<Vec<(String, DestroyCause)>>,
}

impl DestructionPort for FakeDestruction {
    fn destroy_resident_task(&self, task_id: &str, cause: DestroyCause) -> Result<(), HostError> {
        lock(&self.calls).push((task_id.to_string(), cause));
        Ok(())
    }
}

type LiveMap = Shared<BTreeMap<String, Arc<dyn ManagedChildHandle>>>;

struct Harness {
    _dir: tempfile::TempDir,
    engine: SteeringEngine,
    store: TaskRecordStore,
    destruction: Arc<FakeDestruction>,
    revive_calls: Shared<Vec<String>>,
    dequeue_calls: Shared<Vec<String>>,
    live: LiveMap,
}

fn port_for(
    store: &TaskRecordStore,
    live: &LiveMap,
    destruction: Arc<FakeDestruction>,
    revive_calls: &Shared<Vec<String>>,
    dequeue_calls: &Shared<Vec<String>>,
    dequeue_result: bool,
) -> SteeringPort {
    let live = Arc::clone(live);
    let revive_calls = Arc::clone(revive_calls);
    let dequeue_calls = Arc::clone(dequeue_calls);
    SteeringPort {
        store: store.clone(),
        live_handle: Arc::new(move |task_id| lock(&live).get(task_id).cloned()),
        dequeue_pending: Arc::new(move |task_id| {
            lock(&dequeue_calls).push(task_id.to_string());
            dequeue_result
        }),
        reacquire_for_revive: Arc::new(move |task_id| {
            lock(&revive_calls).push(task_id.to_string())
        }),
        destruction,
        run_stats_snapshot: Arc::new(|_| None),
        now: Arc::new(|| 1_783_296_000_000),
    }
}

fn harness_with_dequeue(dequeue_result: bool) -> Harness {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = TaskRecordStore::new(&StateDirConfig {
        project_dir: dir.path().to_path_buf(),
        task_state_dir: None,
    });
    let live: LiveMap = Arc::default();
    let destruction = Arc::new(FakeDestruction::default());
    let revive_calls = Shared::default();
    let dequeue_calls = Shared::default();
    let engine = SteeringEngine::new(port_for(
        &store,
        &live,
        Arc::clone(&destruction),
        &revive_calls,
        &dequeue_calls,
        dequeue_result,
    ));
    Harness {
        _dir: dir,
        engine,
        store,
        destruction,
        revive_calls,
        dequeue_calls,
        live,
    }
}

fn harness() -> Harness {
    harness_with_dequeue(false)
}

impl Harness {
    fn seed(&self, overrides: impl FnOnce(&mut TaskRecordInput)) -> TaskRecord {
        let mut input = TaskRecordInput {
            parent_session_id: "parent-1".to_string(),
            root_session_id: "parent-1".to_string(),
            depth: 1,
            execution_mode: "in-process".to_string(),
            model: "anthropic/claude".to_string(),
            notify_on_terminal: false,
            ..TaskRecordInput::default()
        };
        overrides(&mut input);
        let record = create_task_record(input, None).expect("record");
        self.store.save(&record).expect("save");
        record
    }

    fn seed_record(&self) -> TaskRecord {
        self.seed(|_| {})
    }

    fn set_live(&self, task_id: &str, handle: Arc<dyn ManagedChildHandle>) {
        lock(&self.live).insert(task_id.to_string(), handle);
    }

    fn load(&self, task_id: &str) -> TaskRecord {
        self.store.load(task_id).expect("load").expect("record")
    }

    fn destroy_calls(&self) -> Vec<(String, DestroyCause)> {
        lock(&self.destruction.calls).clone()
    }

    fn transition(&self, task_id: &str, transition: TaskTransition) {
        self.store
            .transition(task_id, &transition)
            .expect("transition");
    }

    fn to_running(&self, record: &TaskRecord) {
        self.transition(
            &record.task_id,
            TaskTransition::Start {
                timestamp: now_iso(),
                pid: None,
                child_session_id: None,
            },
        );
    }

    fn to_completed(&self, record: &TaskRecord) {
        self.to_running(record);
        self.transition(
            &record.task_id,
            TaskTransition::Complete {
                timestamp: now_iso(),
                final_response: "first pass".to_string(),
                run_stats: None,
            },
        );
    }

    /// A fresh store + engine over the SAME state dir (simulated process restart).
    fn restart(&self) -> Harness {
        let store = TaskRecordStore::new(&StateDirConfig {
            project_dir: std::path::PathBuf::new(),
            task_state_dir: Some(self.store.state_dir().to_path_buf()),
        });
        let live: LiveMap = Arc::default();
        let destruction = Arc::new(FakeDestruction::default());
        let revive_calls = Shared::default();
        let dequeue_calls = Shared::default();
        let engine = SteeringEngine::new(port_for(
            &store,
            &live,
            Arc::clone(&destruction),
            &revive_calls,
            &dequeue_calls,
            false,
        ));
        Harness {
            _dir: tempfile::tempdir().expect("tempdir"),
            engine,
            store,
            destruction,
            revive_calls,
            dequeue_calls,
            live,
        }
    }
}

fn send(harness: &Harness, input: SendInput) -> SendOutcome {
    harness.engine.send_to_task(&input).expect("send")
}

fn steer_input(id: &str, message: &str) -> SendInput {
    SendInput {
        deliver_as: Some(DeliverAs::Steer),
        ..SendInput::new(id, message)
    }
}

fn cancel(harness: &Harness, id: &str, reason: Option<&str>) -> CancelOutcome {
    harness
        .engine
        .cancel_task(id, reason, CancelOptions::default())
        .expect("cancel")
}

fn pending_len(record: &TaskRecord) -> usize {
    record.pending_steering.as_ref().map_or(0, Vec::len)
}

#[test]
fn running_resident_child_steer_lands_mid_run_on_the_live_handle() {
    for flavor in FLAVORS {
        let h = harness();
        let record = h.seed_record();
        h.to_running(&record);
        let fake = FakeHandle::new(&record.task_id, flavor);
        h.set_live(&record.task_id, fake.clone());
        let outcome = send(&h, steer_input(&record.task_id, "keep going"));
        let SendOutcome::Steered { delivered, .. } = outcome else {
            panic!("expected steered, got {outcome:?}");
        };
        assert_eq!(delivered, DeliverAs::Steer);
        assert_eq!(fake.steers(), vec!["keep going"]);
    }
}

#[test]
fn completed_resident_child_revives_on_the_same_instance_with_incremented_epoch() {
    for flavor in FLAVORS {
        let h = harness();
        let record = h.seed_record();
        h.to_completed(&record);
        let fake = FakeHandle::new(&record.task_id, flavor);
        h.set_live(&record.task_id, fake.clone());
        let outcome = send(&h, SendInput::new(&record.task_id, "second pass"));
        let SendOutcome::Revived { run_epoch, .. } = outcome else {
            panic!("expected revived, got {outcome:?}");
        };
        assert_eq!(run_epoch, 1);
        assert_eq!(fake.follow_ups(), vec!["second pass"]);
        let revived = h.load(&record.task_id);
        assert_eq!(revived.status, TaskStatus::Running);
        assert_eq!(
            revived.residency_state,
            crate::state::ResidencyState::Resident
        );
        assert_eq!(revived.notification.run_epoch, 1);
        assert!(lock(&h.revive_calls).contains(&record.task_id));
    }
}

#[test]
fn interrupt_keeps_partial_text_and_a_later_send_revives() {
    for flavor in FLAVORS {
        let h = harness();
        let record = h.seed_record();
        h.to_running(&record);
        let fake = FakeHandle::new(&record.task_id, flavor);
        *lock(&fake.last_text) = Some("partial answer so far".to_string());
        h.set_live(&record.task_id, fake.clone());
        let interrupted = h.engine.interrupt_task(&record.task_id).expect("interrupt");
        let InterruptOutcome::Interrupted {
            previous_status, ..
        } = interrupted
        else {
            panic!("expected interrupted, got {interrupted:?}");
        };
        assert_eq!(previous_status, TaskStatus::Running);
        assert_eq!(fake.aborts(), 1);
        let after = h.load(&record.task_id);
        assert_eq!(after.status, TaskStatus::Interrupted);
        assert_eq!(
            after.final_response.as_deref(),
            Some("partial answer so far")
        );
        let sent = send(&h, SendInput::new(&record.task_id, "resume please"));
        assert!(matches!(sent, SendOutcome::Revived { .. }), "{sent:?}");
        assert_eq!(fake.follow_ups(), vec!["resume please"]);
    }
}

#[test]
fn cancel_runs_destruction_once_and_a_later_send_is_not_continuable() {
    for flavor in FLAVORS {
        let h = harness();
        let record = h.seed_record();
        h.to_running(&record);
        let fake = FakeHandle::new(&record.task_id, flavor);
        h.set_live(&record.task_id, fake.clone());
        let cancelled = cancel(&h, &record.task_id, Some("user aborted"));
        let CancelOutcome::Cancelled {
            previous_status, ..
        } = cancelled
        else {
            panic!("expected cancelled, got {cancelled:?}");
        };
        assert_eq!(previous_status, TaskStatus::Running);
        assert_eq!(fake.aborts(), 1);
        assert_eq!(
            h.destroy_calls(),
            vec![(record.task_id.clone(), DestroyCause::Cancel)]
        );
        assert_eq!(h.load(&record.task_id).status, TaskStatus::Cancelled);
        let sent = send(&h, SendInput::new(&record.task_id, "one more"));
        assert!(
            matches!(sent, SendOutcome::NotContinuable { .. }),
            "{sent:?}"
        );
    }
}

#[test]
fn cancel_whose_abort_rejects_still_destroys_exactly_once() {
    for flavor in FLAVORS {
        let h = harness();
        let record = h.seed_record();
        h.to_running(&record);
        let fake = Arc::new(FakeHandle::build(&record.task_id, flavor, true));
        h.set_live(&record.task_id, fake.clone());
        let cancelled = cancel(&h, &record.task_id, Some("user aborted"));
        let CancelOutcome::Cancelled {
            previous_status, ..
        } = cancelled
        else {
            panic!("expected cancelled, got {cancelled:?}");
        };
        assert_eq!(previous_status, TaskStatus::Running);
        assert_eq!(fake.aborts(), 1);
        assert_eq!(
            h.destroy_calls(),
            vec![(record.task_id.clone(), DestroyCause::Cancel)]
        );
        assert_eq!(h.load(&record.task_id).status, TaskStatus::Cancelled);
    }
}

#[test]
fn second_cancel_is_an_idempotent_noop_without_rerunning_destruction() {
    for flavor in FLAVORS {
        let h = harness();
        let record = h.seed_record();
        h.to_running(&record);
        h.set_live(&record.task_id, FakeHandle::new(&record.task_id, flavor));
        cancel(&h, &record.task_id, None);
        let second = cancel(&h, &record.task_id, None);
        assert!(matches!(second, CancelOutcome::Noop { .. }), "{second:?}");
        assert_eq!(h.destroy_calls().len(), 1);
    }
}

#[test]
fn pending_child_queues_two_messages_and_delivers_in_order_after_start() {
    for flavor in FLAVORS {
        let h = harness();
        let record = h.seed_record();
        let first = send(&h, SendInput::new(&record.task_id, "first"));
        let second = send(&h, SendInput::new(&record.task_id, "second"));
        assert!(
            matches!(
                first,
                SendOutcome::Queued {
                    queue_position: 1,
                    ..
                }
            ),
            "{first:?}"
        );
        assert!(
            matches!(
                second,
                SendOutcome::Queued {
                    queue_position: 2,
                    ..
                }
            ),
            "{second:?}"
        );
        let fake = FakeHandle::new(&record.task_id, flavor);
        h.set_live(&record.task_id, fake.clone());
        h.engine.notify_started(&record.task_id).expect("notify");
        assert_eq!(fake.follow_ups(), vec!["first", "second"]);
    }
}

#[test]
fn cross_session_send_without_all_scope_is_scope_denied_naming_the_owner() {
    let h = harness();
    let record = h.seed_record();
    h.to_running(&record);
    h.set_live(
        &record.task_id,
        FakeHandle::new(&record.task_id, Flavor::InProcess),
    );
    let outcome = send(
        &h,
        SendInput {
            caller_session_id: Some("parent-2".to_string()),
            ..SendInput::new(&record.task_id, "hi")
        },
    );
    let SendOutcome::ScopeDenied {
        owning_session_id, ..
    } = outcome
    else {
        panic!("expected scope_denied, got {outcome:?}");
    };
    assert_eq!(owning_session_id, "parent-1");
}

#[test]
fn cross_session_send_with_all_scope_is_allowed() {
    let h = harness();
    let record = h.seed_record();
    h.to_running(&record);
    h.set_live(
        &record.task_id,
        FakeHandle::new(&record.task_id, Flavor::InProcess),
    );
    let outcome = send(
        &h,
        SendInput {
            caller_session_id: Some("parent-2".to_string()),
            all_scope: true,
            ..SendInput::new(&record.task_id, "hi")
        },
    );
    assert!(
        matches!(outcome, SendOutcome::Steered { .. }),
        "{outcome:?}"
    );
}

#[test]
fn unknown_selector_reports_not_found_with_tasks_suggestion() {
    let h = harness();
    let outcome = send(&h, SendInput::new("st_0000dead", "hi"));
    let SendOutcome::NotFound { suggestion, .. } = outcome else {
        panic!("expected not_found, got {outcome:?}");
    };
    assert!(suggestion.contains("/tasks"));
    assert!(suggestion.contains("task_output"));
    assert!(!suggestion.contains("task_list"));
}

#[test]
fn name_selector_resolves_to_the_record() {
    let h = harness();
    let record = h.seed(|input| input.name = Some("researcher".to_string()));
    h.to_running(&record);
    let fake = FakeHandle::new(&record.task_id, Flavor::InProcess);
    h.set_live(&record.task_id, fake.clone());
    let outcome = send(&h, steer_input("researcher", "go"));
    assert!(
        matches!(outcome, SendOutcome::Steered { .. }),
        "{outcome:?}"
    );
    assert_eq!(fake.steers(), vec!["go"]);
}

#[test]
fn pending_cancel_drops_the_queue_without_delivery() {
    let h = harness_with_dequeue(true);
    let record = h.seed_record();
    send(&h, SendInput::new(&record.task_id, "first"));
    send(&h, SendInput::new(&record.task_id, "second"));
    let cancelled = cancel(&h, &record.task_id, Some("not needed"));
    let fake = FakeHandle::new(&record.task_id, Flavor::InProcess);
    h.set_live(&record.task_id, fake.clone());
    h.engine.notify_started(&record.task_id).expect("notify");
    let CancelOutcome::Cancelled {
        previous_status, ..
    } = cancelled
    else {
        panic!("expected cancelled, got {cancelled:?}");
    };
    assert_eq!(previous_status, TaskStatus::Pending);
    assert_eq!(h.load(&record.task_id).status, TaskStatus::Cancelled);
    assert_eq!(*lock(&h.dequeue_calls), vec![record.task_id.clone()]);
    assert_eq!(
        h.destroy_calls(),
        vec![(record.task_id.clone(), DestroyCause::Cancel)]
    );
    assert!(fake.follow_ups().is_empty());
    assert!(fake.steers().is_empty());
}

#[test]
fn pending_interrupt_is_a_noop_and_stays_pending() {
    let h = harness();
    let record = h.seed_record();
    let interrupted = h.engine.interrupt_task(&record.task_id).expect("interrupt");
    assert!(
        matches!(interrupted, InterruptOutcome::Noop { .. }),
        "{interrupted:?}"
    );
    assert_eq!(h.load(&record.task_id).status, TaskStatus::Pending);
}

#[test]
fn drop_pending_discards_buffered_messages() {
    let h = harness();
    let record = h.seed_record();
    send(&h, SendInput::new(&record.task_id, "first"));
    send(&h, SendInput::new(&record.task_id, "second"));
    assert_eq!(pending_len(&h.load(&record.task_id)), 2);
    h.engine.drop_pending(&record.task_id).expect("drop");
    let fake = FakeHandle::new(&record.task_id, Flavor::InProcess);
    h.set_live(&record.task_id, fake.clone());
    h.engine.notify_started(&record.task_id).expect("notify");
    assert!(fake.follow_ups().is_empty());
    assert!(fake.steers().is_empty());
    assert_eq!(pending_len(&h.load(&record.task_id)), 0);
}

#[test]
fn durable_queue_drains_in_persisted_order_after_restart() {
    let h = harness();
    let record = h.seed_record();
    let first = send(&h, steer_input(&record.task_id, "first"));
    let second = send(&h, SendInput::new(&record.task_id, "second"));
    assert!(
        matches!(
            first,
            SendOutcome::Queued {
                queue_position: 1,
                ..
            }
        ),
        "{first:?}"
    );
    assert!(
        matches!(
            second,
            SendOutcome::Queued {
                queue_position: 2,
                ..
            }
        ),
        "{second:?}"
    );
    let persisted = h.load(&record.task_id).pending_steering.expect("queue");
    let messages: Vec<&str> = persisted
        .iter()
        .map(|entry| entry.message.as_str())
        .collect();
    assert_eq!(messages, vec!["first", "second"]);
    assert_eq!(persisted[0].deliver_as, DeliverAs::Steer);
    assert_eq!(persisted[1].deliver_as, DeliverAs::FollowUp);
    let restarted = h.restart();
    let tracked = FakeHandle::new(&record.task_id, Flavor::InProcess);
    restarted.set_live(&record.task_id, tracked.clone());
    restarted
        .engine
        .notify_started(&record.task_id)
        .expect("notify");
    assert_eq!(tracked.order(), vec!["steer:first", "followUp:second"]);
    assert_eq!(pending_len(&restarted.load(&record.task_id)), 0);
}

#[test]
fn pending_cancel_clears_persisted_queue_so_a_post_restart_start_delivers_nothing() {
    let h = harness();
    let record = h.seed_record();
    send(&h, SendInput::new(&record.task_id, "first"));
    send(&h, SendInput::new(&record.task_id, "second"));
    assert_eq!(pending_len(&h.load(&record.task_id)), 2);
    let cancelled = cancel(&h, &record.task_id, Some("not needed"));
    assert!(
        matches!(cancelled, CancelOutcome::Cancelled { .. }),
        "{cancelled:?}"
    );
    assert_eq!(pending_len(&h.load(&record.task_id)), 0);
    let restarted = h.restart();
    let tracked = FakeHandle::new(&record.task_id, Flavor::InProcess);
    restarted.set_live(&record.task_id, tracked.clone());
    restarted
        .engine
        .notify_started(&record.task_id)
        .expect("notify");
    assert!(tracked.order().is_empty());
}

fn assert_suspended_send(transition: TaskTransition) {
    let h = harness();
    let record = h.seed_record();
    h.to_running(&record);
    h.transition(&record.task_id, transition);
    let outcome = send(&h, SendInput::new(&record.task_id, "wake up"));
    let SendOutcome::NotContinuable { reason, .. } = outcome else {
        panic!("expected not_continuable, got {outcome:?}");
    };
    assert!(reason.contains("suspended - resumes when its session is resumed"));
    assert_eq!(h.load(&record.task_id).status, TaskStatus::Running);
}

#[test]
fn persisted_only_suspended_child_is_not_continuable_without_revive() {
    assert_suspended_send(TaskTransition::PersistOnly {
        timestamp: now_iso(),
    });
}

#[test]
fn rpc_detached_suspended_child_is_not_continuable() {
    assert_suspended_send(TaskTransition::DetachRpc {
        timestamp: now_iso(),
    });
}

#[test]
fn malformed_persisted_entry_is_dropped_and_the_rest_deliver_in_order() {
    let h = harness();
    let record = h.seed_record();
    let path = h
        .store
        .state_dir()
        .join("tasks")
        .join(format!("{}.json", record.task_id));
    let mut on_disk: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).expect("read")).expect("json");
    on_disk["pending_steering"] = serde_json::json!([
        { "id": "ps-1", "message": "first", "deliver_as": "followUp" },
        { "id": "ps-2" },
        { "id": "ps-3", "message": "third", "deliver_as": "steer" },
    ]);
    std::fs::write(&path, on_disk.to_string()).expect("write");
    let restarted = h.restart();
    let listed = restarted.store.list().expect("list");
    assert!(
        listed
            .records
            .iter()
            .any(|entry| entry.task_id == record.task_id)
    );
    assert!(listed.diagnostics.iter().any(|diagnostic| matches!(
        diagnostic,
        TaskRecordDiagnostic::ParseWarning { message, .. } if message.contains("pending_steering[1]")
    )));
    let tracked = FakeHandle::new(&record.task_id, Flavor::InProcess);
    restarted.set_live(&record.task_id, tracked.clone());
    restarted
        .engine
        .notify_started(&record.task_id)
        .expect("notify");
    assert_eq!(tracked.order(), vec!["followUp:first", "steer:third"]);
    assert_eq!(pending_len(&restarted.load(&record.task_id)), 0);
}

fn momus_reminder() -> &'static str {
    interaction_policy_for_agent("momus")
        .expect("momus policy")
        .send_denial_reminder
}

fn seed_momus(h: &Harness) -> TaskRecord {
    h.seed(|input| input.agent_type = Some("momus".to_string()))
}

#[test]
fn running_momus_child_refuses_send_with_registry_reminder() {
    let h = harness();
    let record = seed_momus(&h);
    h.to_running(&record);
    let fake = FakeHandle::new(&record.task_id, Flavor::InProcess);
    h.set_live(&record.task_id, fake.clone());
    let outcome = send(&h, steer_input(&record.task_id, "steer attempt"));
    assert_eq!(
        outcome,
        SendOutcome::OneShotAgent {
            task_id: record.task_id.clone(),
            agent: "momus".to_string(),
            message: momus_reminder().to_string(),
        }
    );
    assert!(fake.steers().is_empty());
    assert!(fake.follow_ups().is_empty());
}

#[test]
fn pending_momus_child_refuses_send_and_queues_nothing() {
    let h = harness();
    let record = seed_momus(&h);
    let outcome = send(&h, SendInput::new(&record.task_id, "pre-launch note"));
    let SendOutcome::OneShotAgent { message, .. } = outcome else {
        panic!("expected one_shot_agent, got {outcome:?}");
    };
    assert_eq!(message, momus_reminder());
    assert_eq!(pending_len(&h.load(&record.task_id)), 0);
}

#[test]
fn completed_momus_child_refuses_send_without_revive() {
    let h = harness();
    let record = seed_momus(&h);
    h.to_completed(&record);
    let fake = FakeHandle::new(&record.task_id, Flavor::InProcess);
    h.set_live(&record.task_id, fake.clone());
    let outcome = send(&h, SendInput::new(&record.task_id, "revive attempt"));
    let SendOutcome::OneShotAgent { message, .. } = outcome else {
        panic!("expected one_shot_agent, got {outcome:?}");
    };
    assert_eq!(message, momus_reminder());
    assert!(fake.follow_ups().is_empty());
    assert!(lock(&h.revive_calls).is_empty());
    assert_eq!(h.load(&record.task_id).status, TaskStatus::Completed);
}

#[test]
fn foreign_momus_child_without_all_scope_is_scope_denied_first() {
    let h = harness();
    let record = seed_momus(&h);
    h.to_running(&record);
    h.set_live(
        &record.task_id,
        FakeHandle::new(&record.task_id, Flavor::InProcess),
    );
    let outcome = send(
        &h,
        SendInput {
            caller_session_id: Some("parent-2".to_string()),
            ..SendInput::new(&record.task_id, "hi")
        },
    );
    let SendOutcome::ScopeDenied {
        owning_session_id, ..
    } = outcome
    else {
        panic!("expected scope_denied, got {outcome:?}");
    };
    assert_eq!(owning_session_id, "parent-1");
}

#[test]
fn foreign_momus_child_with_all_scope_gets_the_one_shot_refusal() {
    let h = harness();
    let record = seed_momus(&h);
    h.to_running(&record);
    let fake = FakeHandle::new(&record.task_id, Flavor::InProcess);
    h.set_live(&record.task_id, fake.clone());
    let outcome = send(
        &h,
        SendInput {
            caller_session_id: Some("parent-2".to_string()),
            all_scope: true,
            ..SendInput::new(&record.task_id, "hi")
        },
    );
    let SendOutcome::OneShotAgent { message, .. } = outcome else {
        panic!("expected one_shot_agent, got {outcome:?}");
    };
    assert_eq!(message, momus_reminder());
    assert!(fake.steers().is_empty());
}
