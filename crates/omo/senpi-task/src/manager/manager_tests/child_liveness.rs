//! `manager/child-liveness.test.ts` and `rpc-host/handle-liveness.test.ts` (manager side):
//! records carry the live host liveness, a non-running child's sends are held, a terminal-pending
//! notice reaches the parent at once, and a return to running drains the held messages.

use std::sync::{Arc, Mutex};

use super::fakes::{HarnessOptions, base_spec, make_manager, started, wait_until};
use crate::state::TaskStatus;
use crate::state::host_liveness::HostLivenessStatus;
use crate::steering::{SendInput, SendOutcome};

#[test]
fn an_unresponsive_child_holds_sends_notifies_the_parent_and_drains_on_recovery() {
    let notices = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&notices);
    let harness = make_manager(HarnessOptions {
        customize: Some(Box::new(move |options| {
            options.on_child_liveness = Some(Arc::new(move |record, status| {
                sink.lock()
                    .expect("notices")
                    .push((record.task_id.clone(), status));
            }));
        })),
        ..HarnessOptions::default()
    });
    let task = started(harness.manager.start(&base_spec()));
    let fake = harness.in_process.wait_handle(&task.task_id);
    wait_until("running", || {
        harness
            .manager
            .get(&task.task_id)
            .is_some_and(|record| record.status == TaskStatus::Running)
    });

    fake.publish_liveness(HostLivenessStatus::Unresponsive { since: 5 });
    assert_eq!(
        harness
            .manager
            .get(&task.task_id)
            .expect("record")
            .host_liveness,
        Some(HostLivenessStatus::Unresponsive { since: 5 })
    );
    let listed = harness.manager.list(&crate::manager::types::ListScope::All);
    assert_eq!(
        listed[0].record.host_liveness,
        Some(HostLivenessStatus::Unresponsive { since: 5 })
    );
    let held = harness
        .manager
        .send_to_task(&SendInput::new(&task.task_id, "queued"))
        .expect("send");
    assert!(matches!(held, SendOutcome::Queued { .. }), "{held:?}");
    assert!(fake.steer_calls().is_empty() && fake.follow_up_calls().is_empty());

    fake.publish_liveness(HostLivenessStatus::TerminalPending { since: 5 });
    assert_eq!(
        *notices.lock().expect("notices"),
        vec![(
            task.task_id.clone(),
            HostLivenessStatus::TerminalPending { since: 5 }
        )]
    );

    fake.publish_liveness(HostLivenessStatus::Running);
    assert_eq!(fake.follow_up_calls(), vec!["queued".to_string()]);
    assert!(
        harness
            .store
            .load(&task.task_id)
            .expect("load")
            .expect("record")
            .pending_steering
            .is_none()
    );
}

type Timers = Arc<Mutex<Vec<Box<dyn FnOnce() + Send>>>>;

/// `child-liveness.test.ts` `observedChild`: a started child whose debounce timers the test fires
/// by hand (the fake clock's `advance(30_000)`), recording each parent notice.
struct Observed {
    harness: super::fakes::Harness,
    task_id: String,
    fake: Arc<super::fakes::FakeHandle>,
    notices: Arc<Mutex<Vec<&'static str>>>,
    timers: Timers,
}

impl Observed {
    fn new() -> Self {
        let notices = Arc::new(Mutex::new(Vec::new()));
        let timers: Timers = Arc::default();
        let sink = Arc::clone(&notices);
        let scheduled = Arc::clone(&timers);
        let harness = make_manager(HarnessOptions {
            customize: Some(Box::new(move |options| {
                options.on_child_liveness = Some(Arc::new(move |_record, status| {
                    sink.lock().expect("notices").push(match status {
                        HostLivenessStatus::Running => "running",
                        HostLivenessStatus::Unresponsive { .. } => "unresponsive",
                        HostLivenessStatus::TerminalPending { .. } => "terminal-pending",
                    });
                }));
                options.liveness_notice_timer = Some(Arc::new(move |notify| {
                    scheduled.lock().expect("timers").push(notify);
                }));
            })),
            ..HarnessOptions::default()
        });
        let task = started(harness.manager.start(&base_spec()));
        let fake = harness.in_process.wait_handle(&task.task_id);
        wait_until("running", || {
            harness
                .manager
                .get(&task.task_id)
                .is_some_and(|record| record.status == TaskStatus::Running)
        });
        Self {
            harness,
            task_id: task.task_id,
            fake,
            notices,
            timers,
        }
    }

    /// Every pending debounce window elapses.
    fn advance(&self) {
        let due: Vec<_> = self.timers.lock().expect("timers").drain(..).collect();
        for notify in due {
            notify();
        }
    }

    fn notices(&self) -> Vec<&'static str> {
        self.notices.lock().expect("notices").clone()
    }

    fn complete(&self) {
        self.fake.complete("finished first turn");
        super::fakes::wait_terminal(&self.harness.manager, &self.task_id);
    }

    fn send(&self) -> SendOutcome {
        self.harness
            .manager
            .send_to_task(&SendInput::new(&self.task_id, "continue"))
            .expect("send")
    }
}

#[test]
fn a_completed_resident_child_has_a_retryable_unresponsive_send_result_and_can_recover() {
    let child = Observed::new();
    child.complete();
    child
        .fake
        .publish_liveness(HostLivenessStatus::Unresponsive { since: 1 });

    let refused = child.send();
    let SendOutcome::Unresponsive {
        task_id, retryable, ..
    } = refused
    else {
        panic!("expected unresponsive, got {refused:?}");
    };
    assert_eq!(task_id, child.task_id);
    assert!(retryable);
    assert!(child.fake.follow_up_calls().is_empty());
    assert_eq!(
        child
            .harness
            .manager
            .get(&child.task_id)
            .expect("record")
            .status,
        TaskStatus::Completed
    );

    child.fake.publish_liveness(HostLivenessStatus::Running);
    let resumed = child.send();
    assert!(
        matches!(resumed, SendOutcome::Revived { .. }),
        "{resumed:?}"
    );
    assert_eq!(child.fake.follow_up_calls(), vec!["continue".to_string()]);
}

#[test]
fn a_completed_resident_child_still_reports_persistent_unresponsiveness() {
    let child = Observed::new();
    child.complete();
    child
        .fake
        .publish_liveness(HostLivenessStatus::Unresponsive { since: 1 });
    assert!(child.notices().is_empty(), "notice waits for the debounce");
    child.advance();
    assert_eq!(child.notices(), vec!["unresponsive"]);
}

#[test]
fn brief_liveness_oscillations_do_not_wake_the_parent_but_stable_transitions_do() {
    let child = Observed::new();
    for since in 0..12 {
        child
            .fake
            .publish_liveness(HostLivenessStatus::Unresponsive { since });
        child.fake.publish_liveness(HostLivenessStatus::Running);
        child.advance();
    }
    assert!(child.notices().is_empty(), "{:?}", child.notices());

    child
        .fake
        .publish_liveness(HostLivenessStatus::Unresponsive { since: 99 });
    child.advance();
    child.fake.publish_liveness(HostLivenessStatus::Running);
    child.advance();
    assert_eq!(child.notices(), vec!["unresponsive", "running"]);
}

#[test]
fn forgetting_a_child_cancels_its_pending_liveness_notice() {
    let child = Observed::new();
    child
        .fake
        .publish_liveness(HostLivenessStatus::Unresponsive { since: 0 });
    child.harness.manager.forget(&child.task_id);
    child.advance();
    assert!(child.notices().is_empty(), "{:?}", child.notices());
}

/// `child-liveness.test.ts` "terminal-pending bypasses the transient-status debounce": the notice
/// is immediate, and the pending `unresponsive` window it superseded never fires after it.
#[test]
fn terminal_pending_bypasses_the_transient_status_debounce() {
    let child = Observed::new();
    child
        .fake
        .publish_liveness(HostLivenessStatus::Unresponsive { since: 0 });
    child
        .fake
        .publish_liveness(HostLivenessStatus::TerminalPending { since: 0 });
    assert_eq!(child.notices(), vec!["terminal-pending"]);
    child.advance();
    assert_eq!(child.notices(), vec!["terminal-pending"]);
}
