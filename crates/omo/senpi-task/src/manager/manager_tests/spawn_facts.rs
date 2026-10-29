//! `manager/manager-spawn-facts.test.ts`.

use std::collections::BTreeMap;

use serde_json::{Value, json};

use super::fakes::{FakeRunner, HarnessOptions, base_spec, make_manager, started};
use crate::manager::execution_mode::ExecutionMode;
use crate::manager::helpers::record_spawned_pid;
use crate::manager::types::ManagerStartSpec;
use crate::state::TaskRecord;
use crate::store::parse_task_record;

fn running_record(status: &str) -> TaskRecord {
    let raw: Value = json!({
        "version": 1,
        "task_id": "st_deadbeef",
        "name": "t",
        "parent_session_id": "parent-1",
        "root_session_id": "parent-1",
        "depth": 1,
        "execution_mode": "process",
        "model": "omo-mock/mock-1",
        "status": status,
        "residency_state": "resident",
        "created_at": "2026-07-07T00:00:00.000Z",
        "updated_at": "2026-07-07T00:00:00.000Z",
        "notify_on_terminal": false,
        "notification": { "run_epoch": 0, "notified_epoch": 0 },
    });
    parse_task_record(&raw, "/tmp/record.json", &mut Vec::new()).expect("parse")
}

fn process_spec() -> ManagerStartSpec {
    ManagerStartSpec {
        execution_mode: Some(ExecutionMode::Process),
        ..base_spec()
    }
}

#[test]
fn given_running_record_and_real_pid_when_folded_then_pid_set_on_copy() {
    let record = running_record("running");

    let updated = record_spawned_pid(&record, Some(4242));

    assert_eq!(updated.and_then(|updated| updated.pid), Some(4242));
    assert_eq!(record.pid, None);
}

#[test]
fn given_no_pid_when_folded_then_nothing_changes() {
    assert_eq!(record_spawned_pid(&running_record("running"), None), None);
}

#[test]
fn given_record_already_terminal_when_folded_then_left_untouched() {
    assert_eq!(
        record_spawned_pid(&running_record("completed"), Some(4242)),
        None
    );
}

#[test]
fn given_member_launch_env_when_process_task_starts_then_task_id_threaded_into_child_env() {
    let process = FakeRunner::new();
    let harness = make_manager(HarnessOptions {
        process: Some(process.clone()),
        ..HarnessOptions::default()
    });
    let member_env = BTreeMap::from([
        (
            "SENPI_TASK_MEMBER".to_string(),
            "11111111-1111-4111-8111-111111111111::alpha".to_string(),
        ),
        ("SENPI_TASK_TEAM_CONFIG".to_string(), "{}".to_string()),
    ]);

    let task = started(harness.manager.start(&ManagerStartSpec {
        member_env: Some(member_env.clone()),
        ..process_spec()
    }));

    let mut expected = member_env;
    expected.insert("SENPI_TASK_MEMBER_TASK_ID".to_string(), task.task_id);
    let specs = process.specs();
    assert_eq!(
        specs.first().and_then(|spec| spec.member_env.clone()),
        Some(expected)
    );
}

#[test]
fn given_process_runner_with_pid_when_process_task_starts_then_record_carries_pid() {
    let process = FakeRunner::new();
    *process.child_pid.lock().expect("pid") = Some(4242);
    let harness = make_manager(HarnessOptions {
        process: Some(process.clone()),
        ..HarnessOptions::default()
    });

    let task = started(harness.manager.start(&process_spec()));

    assert_eq!(process.specs().len(), 1);
    let record = harness
        .store
        .load(&task.task_id)
        .expect("load")
        .expect("record");
    assert_eq!(record.pid, Some(4242));
}

#[test]
fn given_in_process_runner_without_pid_when_task_starts_then_record_has_no_pid() {
    let harness = make_manager(HarnessOptions::default());

    let task = started(harness.manager.start(&ManagerStartSpec {
        execution_mode: Some(ExecutionMode::InProcess),
        ..base_spec()
    }));

    let record = harness
        .store
        .load(&task.task_id)
        .expect("load")
        .expect("record");
    assert_eq!(record.pid, None);
}
