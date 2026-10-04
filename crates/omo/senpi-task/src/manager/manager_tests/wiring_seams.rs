//! `manager/manager-wiring-seams.test.ts`.

use std::sync::Arc;

use super::fakes::{HarnessOptions, base_spec, default_manager, make_manager, named, started};
use crate::manager::types::{ManagerStartSpec, SpawnAdmission, StartResult, TaskManagerOptions};

fn background(spec: ManagerStartSpec, run_in_background: bool) -> ManagerStartSpec {
    ManagerStartSpec {
        run_in_background,
        ..spec
    }
}

fn with_admission(admission: SpawnAdmission) -> super::fakes::Harness {
    make_manager(HarnessOptions {
        customize: Some(Box::new(move |options: &mut TaskManagerOptions| {
            options.admit = Some(Arc::new(move |_parent: &str| admission.clone()));
        })),
        ..HarnessOptions::default()
    })
}

#[test]
fn admission_bookkeeping_error_is_not_a_retryable_capacity_denial() {
    let harness = make_manager(HarnessOptions {
        customize: Some(Box::new(|options| {
            options.fallible_admit = Some(Arc::new(|_| Err(crate::host::HostError { message: "approval store unavailable".into() })));
        })),
        ..HarnessOptions::default()
    });
    let result = harness.manager.start(&base_spec());
    let StartResult::StartFailed(failure) = result else { panic!("expected launch failure"); };
    assert_eq!(failure.error_message, "host call failed: approval store unavailable");
    assert_eq!(harness.in_process.started_count(), 0);
    assert!(harness.manager.list(&crate::manager::types::ListScope::All).is_empty());
}

#[test]
fn given_launched_task_when_forgotten_then_live_handle_pruned() {
    let harness = default_manager();
    let task = started(harness.manager.start(&background(base_spec(), true)));
    harness.in_process.wait_handle(&task.task_id);
    assert!(harness.manager.resident_task_ids().contains(&task.task_id));
    assert!(harness.manager.get_resident_handle(&task.task_id).is_some());

    harness.manager.forget(&task.task_id);

    assert!(!harness.manager.resident_task_ids().contains(&task.task_id));
    assert!(harness.manager.get_resident_handle(&task.task_id).is_none());
}

#[test]
fn given_background_and_sync_spawns_when_queried_then_was_background_reflects_spec() {
    let harness = default_manager();
    let bg = started(harness.manager.start(&background(named("bg"), true)));
    let sync = started(harness.manager.start(&background(named("sync"), false)));

    assert!(harness.manager.was_background(&bg.task_id));
    assert!(!harness.manager.was_background(&sync.task_id));
}

#[test]
fn given_foreground_spawn_when_promoted_twice_then_first_flips_and_second_is_idempotent() {
    let harness = default_manager();
    let task = started(harness.manager.start(&background(base_spec(), false)));
    assert!(!harness.manager.was_background(&task.task_id));

    let first = harness.manager.promote_to_background(&task.task_id);
    let second = harness.manager.promote_to_background(&task.task_id);

    assert!(first);
    assert!(!second);
    assert!(harness.manager.was_background(&task.task_id));
}

#[test]
fn given_admit_gate_that_rejects_when_starting_then_residency_denied_and_never_launches() {
    let harness = with_admission(SpawnAdmission::Rejected {
        message: "cap reached".to_string(),
    });

    let result = harness.manager.start(&background(base_spec(), true));

    let StartResult::ResidencyDenied { reason } = result else {
        panic!("expected residency_denied, got {result:?}");
    };
    assert!(reason.contains("cap reached"), "{reason}");
    assert_eq!(harness.in_process.started_count(), 0);
}

#[test]
fn given_admit_gate_that_evicts_when_starting_then_start_proceeds() {
    let harness = with_admission(SpawnAdmission::Evicted {
        evicted_task_id: "st_old".to_string(),
    });

    let result = harness.manager.start(&background(base_spec(), true));

    assert!(matches!(result, StartResult::Started(_)), "{result:?}");
    assert_eq!(harness.in_process.started_count(), 1);
}
