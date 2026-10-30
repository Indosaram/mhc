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
