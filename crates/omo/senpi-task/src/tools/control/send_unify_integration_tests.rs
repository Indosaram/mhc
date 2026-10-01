//! `tools/control/send-unify-integration.test.ts`

use pretty_assertions::assert_eq;

use crate::manager::manager_tests::fakes::{
    HarnessOptions, base_spec, make_manager, started, wait_terminal, wait_until,
};
use crate::manager::types::ManagerStartSpec;
use crate::tools::control::send::run_task_send;
use crate::tools::control::send_schema::{TaskSendInput, TaskSendMessage};

fn is_resident(debug: &str) -> bool {
    matches!(debug, "Resident" | "Some(Resident)")
}

#[test]
fn given_a_completed_resident_child_when_task_send_sends_another_message_then_the_revived_turn_completes()
{
    let harness = make_manager(HarnessOptions::default());
    let manager = &harness.manager;

    let task = started(manager.start(&ManagerStartSpec {
        parent_session_id: "p1".to_string(),
        name: Some("alpha".to_string()),
        ..base_spec()
    }));
    let task_id = task.task_id.clone();
    let fake = harness.in_process.wait_handle(&task_id);

    // First turn settles; the child stays resident after completing.
    fake.complete("first response");
    wait_until("first turn completed and resident", || {
        manager.get(&task_id).is_some_and(|record| {
            record.status.as_str() == "completed"
                && is_resident(&format!("{:?}", record.residency_state))
        })
    });

    let completed_resident = manager.get(&task_id).expect("record present");
    assert_eq!(completed_resident.status.as_str(), "completed");
    assert!(is_resident(&format!(
        "{:?}",
        completed_resident.residency_state
    )));

    let revived = run_task_send(
        manager,
        &TaskSendInput {
            to: task_id.clone(),
            message: Some(TaskSendMessage::Plain(
                "finish with new answer".to_string(),
            )),
            team_run_id: None,
            summary: None,
            all_scope: None,
        },
        Some("p1"),
        None,
    )
    .expect("task_send succeeds");

    assert_eq!(revived.details.kind(), "revived");

    fake.complete("revived final response");
    let completed = wait_terminal(manager, &task_id);
    assert_eq!(completed.status.as_str(), "completed");
    assert_eq!(
        completed.final_response.as_deref(),
        Some("revived final response")
    );
}
