//! `manager/manager-steering-race.test.ts` (with `__fixtures__/race-fakes.ts`) and
//! `manager/manager-revive-slot.test.ts`.

use std::io;
use std::sync::{Arc, Condvar, Mutex};

use serde_json::{Value, json};

use super::fakes::{
    HarnessOptions, Project, base_spec, category_planner, config, lock, make_manager, named,
    started, status_of, wait_terminal, wait_until,
};
use crate::host::HostError;
use crate::lifecycle::DestroyCause;
use crate::manager::continue_result::{ContinueDelivery, ContinueResult};
use crate::manager::types::{
    ManagedRunner, ManagedRunnerResult, ManagedRunners, ManagedStartSpec, TaskManagerOptions,
};
use crate::manager::{ManagedChildHandle, TaskManager, Unsubscribe, create_task_manager};
use crate::runners::in_process::child_handle::{
    ChildSession, ChildSessionListener, InProcessChildHandle,
};
use crate::runners::rpc::handle::{RpcChildHandle, RpcChildHandleOptions};
use crate::runners::rpc::protocol_client::{
    RpcClientError, RpcClientPort, RpcEventListener, RpcExitListener,
};
use crate::runners::types::TerminateOptions;
use crate::state::{DeliverAs, TaskStatus};
use crate::steering::{CancelOptions, CancelOutcome, DestructionPort, InterruptOutcome};
use crate::store::TaskRecordStore;

const RACE_PARTIAL_TEXT: &str = "partial answer so far";

#[derive(Clone, Copy)]
enum Flavor {
    InProcess,
    Rpc,
}

/// `prompt()` and `abort()` resolve off ONE idle signal, so abort settles the launch-time outcome
/// tracker before steering's post-abort transition runs.
#[derive(Default)]
struct OneIdleSession {
    idle: Mutex<bool>,
    fired: Condvar,
}

impl ChildSession for OneIdleSession {
    fn session_id(&self) -> String {
        "sess-race".to_string()
    }

    fn prompt(&self, _text: &str) -> Result<(), HostError> {
        let mut idle = lock(&self.idle);
        while !*idle {
            idle = self
                .fired
                .wait(idle)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        Ok(())
    }

    fn steer(&self, _text: &str) -> Result<(), HostError> {
        Ok(())
    }

    fn follow_up(&self, _text: &str) -> Result<(), HostError> {
        Ok(())
    }

    fn abort(&self) -> Result<(), HostError> {
        *lock(&self.idle) = true;
        self.fired.notify_all();
        Ok(())
    }

    fn subscribe(&self, _listener: ChildSessionListener) -> Unsubscribe {
        Box::new(|| {})
    }

    fn get_last_assistant_text(&self) -> Option<String> {
        Some(RACE_PARTIAL_TEXT.to_string())
    }

    fn dispose(&self) {}
}

/// RPC flavor: the partial assistant message is on the wire; abort ends the agent turn cleanly.
#[derive(Default)]
struct OneIdleRpc {
    listeners: Mutex<Vec<RpcEventListener>>,
}

impl OneIdleRpc {
    fn emit(&self, event: &Value) {
        let listeners: Vec<RpcEventListener> = lock(&self.listeners).clone();
        for listener in listeners {
            listener(event);
        }
    }
}

impl RpcClientPort for OneIdleRpc {
    fn pid(&self) -> Option<u32> {
        Some(4321)
    }

    fn send(&self, command: Value) -> Result<Value, RpcClientError> {
        if command.get("type").and_then(Value::as_str) == Some("abort") {
            self.emit(&json!({ "type": "agent_end", "willRetry": false, "messages": [] }));
        }
        Ok(json!({ "success": true }))
    }

    fn on_event(&self, listener: RpcEventListener) -> Unsubscribe {
        lock(&self.listeners).push(listener);
        Box::new(|| {})
    }

    fn on_exit(&self, _listener: RpcExitListener) -> Unsubscribe {
        Box::new(|| {})
    }

    fn stderr_tail(&self) -> String {
        String::new()
    }

    fn detach(&self) {}

    fn terminate(&self, _options: TerminateOptions) -> io::Result<()> {
        Ok(())
    }
}

struct RaceRunner(Flavor);

impl ManagedRunner for RaceRunner {
    fn start(&self, spec: &ManagedStartSpec) -> ManagedRunnerResult {
        Ok(match self.0 {
            Flavor::InProcess => InProcessChildHandle::start(
                &spec.task_id,
                Arc::new(OneIdleSession::default()),
                &spec.prompt,
            ) as Arc<dyn ManagedChildHandle>,
            Flavor::Rpc => {
                let client = Arc::new(OneIdleRpc::default());
                let handle = RpcChildHandle::new(RpcChildHandleOptions {
                    client: Arc::clone(&client) as Arc<dyn RpcClientPort>,
                    task_id: spec.task_id.clone(),
                    heartbeat_interval_ms: 60_000,
                    now: Arc::new(|| 1),
                });
                client.emit(&json!({
                    "type": "message_end",
                    "message": {
                        "role": "assistant",
                        "content": [{ "type": "text", "text": RACE_PARTIAL_TEXT }],
                    },
                }));
                handle
            }
        })
    }
}

#[derive(Default)]
struct RaceDestruction {
    calls: Mutex<Vec<(String, DestroyCause)>>,
}

impl DestructionPort for RaceDestruction {
    fn destroy_resident_task(&self, task_id: &str, cause: DestroyCause) -> Result<(), HostError> {
        lock(&self.calls).push((task_id.to_string(), cause));
        Ok(())
    }
}

struct RaceHarness {
    manager: TaskManager,
    store: TaskRecordStore,
    destruction: Arc<RaceDestruction>,
    _project: Project,
}

fn race_harness(flavor: Flavor) -> RaceHarness {
    let project = Project::new();
    let store = project.store();
    let destruction = Arc::new(RaceDestruction::default());
    let runner: Arc<dyn ManagedRunner> = Arc::new(RaceRunner(flavor));
    let mut options = TaskManagerOptions::new(
        store.clone(),
        ManagedRunners {
            in_process: Arc::clone(&runner),
            process: runner,
        },
        category_planner(&[]),
        project.cwd(),
    );
    options.config = config(5, 1);
    options.destruction = Some(Arc::clone(&destruction) as Arc<dyn DestructionPort>);
    RaceHarness {
        manager: create_task_manager(options),
        store,
        destruction,
        _project: project,
    }
}

const FLAVORS: [Flavor; 2] = [Flavor::InProcess, Flavor::Rpc];

#[test]
fn given_launch_tracker_settles_on_abort_when_interrupted_then_record_stays_interrupted_with_partial()
 {
    for flavor in FLAVORS {
        let harness = race_harness(flavor);
        let task = started(harness.manager.start(&base_spec()));

        let outcome = harness
            .manager
            .interrupt_task(&task.task_id)
            .expect("interrupt");

        assert!(
            matches!(outcome, InterruptOutcome::Interrupted { .. }),
            "expected interrupted, got {outcome:?}"
        );
        let record = harness
            .store
            .load(&task.task_id)
            .expect("load")
            .expect("record");
        assert_eq!(record.status, TaskStatus::Interrupted);
        assert_eq!(record.final_response.as_deref(), Some(RACE_PARTIAL_TEXT));
    }
}

#[test]
fn given_launch_tracker_settles_on_abort_when_cancelled_then_cancelled_and_destruction_runs_once() {
    for flavor in FLAVORS {
        let harness = race_harness(flavor);
        let task = started(harness.manager.start(&base_spec()));

        let outcome = harness
            .manager
            .cancel_task(
                &task.task_id,
                Some("user aborted"),
                CancelOptions::default(),
            )
            .expect("cancel");

        assert!(matches!(outcome, CancelOutcome::Cancelled { .. }));
        assert_eq!(
            status_of(&harness.store, &task.task_id),
            Some(TaskStatus::Cancelled)
        );
        assert_eq!(
            *lock(&harness.destruction.calls),
            vec![(task.task_id.clone(), DestroyCause::Cancel)]
        );
    }
}

#[test]
fn given_already_cancelled_task_when_cancelled_again_then_noop_and_destruction_not_rerun() {
    for flavor in FLAVORS {
        let harness = race_harness(flavor);
        let task = started(harness.manager.start(&base_spec()));
        harness
            .manager
            .cancel_task(&task.task_id, None, CancelOptions::default())
            .expect("cancel");

        let second = harness
            .manager
            .cancel_task(&task.task_id, None, CancelOptions::default())
            .expect("cancel");

        assert!(matches!(second, CancelOutcome::Noop { .. }));
        assert_eq!(lock(&harness.destruction.calls).len(), 1);
    }
}

#[test]
fn given_already_interrupted_task_when_interrupted_again_then_idempotent_noop() {
    for flavor in FLAVORS {
        let harness = race_harness(flavor);
        let task = started(harness.manager.start(&base_spec()));
        harness
            .manager
            .interrupt_task(&task.task_id)
            .expect("interrupt");

        let second = harness
            .manager
            .interrupt_task(&task.task_id)
            .expect("interrupt");

        assert!(matches!(second, InterruptOutcome::Noop { .. }));
    }
}

#[test]
fn given_single_concurrency_when_revived_task_completes_then_releases_slot_and_queued_starts() {
    let harness = make_manager(HarnessOptions {
        config: Some(config(1, 1)),
        ..HarnessOptions::default()
    });
    let a = started(harness.manager.start(&named("a")));
    let handle = harness.in_process.wait_handle(&a.task_id);
    handle.complete("first");
    assert_eq!(
        wait_terminal(&harness.manager, &a.task_id).status,
        TaskStatus::Completed
    );

    let revived = harness
        .manager
        .continue_task(&a.task_id, "again", Some(DeliverAs::FollowUp))
        .expect("continue");

    let ContinueResult::Continued { delivered, .. } = revived else {
        panic!("expected continued, got {revived:?}");
    };
    assert_eq!(delivered, ContinueDelivery::Revive);
    let record = harness
        .store
        .load(&a.task_id)
        .expect("load")
        .expect("record");
    assert_eq!(record.notification.run_epoch, 1);
    let b = started(harness.manager.start(&named("b")));
    assert_eq!(b.status, TaskStatus::Pending);
    handle.complete("second");
    harness.in_process.wait_handle(&b.task_id);
    wait_until("b running", || {
        status_of(&harness.store, &b.task_id) == Some(TaskStatus::Running)
    });
}

#[test]
fn given_single_concurrency_when_task_interrupted_then_slot_released_for_next_task() {
    let harness = make_manager(HarnessOptions {
        config: Some(config(1, 1)),
        ..HarnessOptions::default()
    });
    let a = started(harness.manager.start(&named("a")));
    harness.in_process.wait_handle(&a.task_id);

    let interrupted = harness
        .manager
        .interrupt_task(&a.task_id)
        .expect("interrupt");

    assert!(matches!(interrupted, InterruptOutcome::Interrupted { .. }));
    let b = started(harness.manager.start(&named("b")));
    assert_eq!(b.status, TaskStatus::Running);
    assert_eq!(
        status_of(&harness.store, &a.task_id),
        Some(TaskStatus::Interrupted)
    );
}
