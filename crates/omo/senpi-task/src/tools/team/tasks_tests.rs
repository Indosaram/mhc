//! `tools/team/tasks.test.ts`

use std::sync::Arc;

use pretty_assertions::assert_eq;
use serde::Serialize;
use serde_json::{Value, json};

use crate::tools::team::tasks::{
    TeamTaskCreateInput, TeamTaskGetInput, TeamTaskListInput, TeamTaskUpdateInput, create_team_task_create_tool,
    create_team_task_get_tool, create_team_task_list_tool, create_team_task_update_tool, run_team_task_create,
    run_team_task_get, run_team_task_list, run_team_task_update,
};
use crate::tools::team::team_tool_fakes::{
    FakeTeamServiceOverrides, create_fake_team_service, fake_task, fake_task_with,
};
use crate::tools::team::types::{TeamToolDeps, TeamToolServiceError, TeamToolsService};

fn result_json<T: Serialize>(result: &T) -> Value {
    serde_json::to_value(result).expect("serialize tool result")
}

fn result_text(value: &Value) -> String {
    value["content"][0]["text"].as_str().unwrap_or("").to_string()
}

fn create_input(value: Value) -> TeamTaskCreateInput {
    serde_json::from_value(value).expect("create input")
}

fn list_input(value: Value) -> TeamTaskListInput {
    serde_json::from_value(value).expect("list input")
}

fn get_input(value: Value) -> TeamTaskGetInput {
    serde_json::from_value(value).expect("get input")
}

fn update_input(value: Value) -> TeamTaskUpdateInput {
    serde_json::from_value(value).expect("update input")
}

fn named_error(name: &str, message: &str) -> TeamToolServiceError {
    let mut error = TeamToolServiceError::new(message.to_string());
    error.name = name.into();
    error
}

fn deps_with_default_service() -> TeamToolDeps {
    let service: Arc<dyn TeamToolsService> =
        Arc::new(create_fake_team_service(FakeTeamServiceOverrides::default()));
    TeamToolDeps { service }
}

fn assert_multiline_subject_collapsed() {
    // given
    let service = create_fake_team_service(FakeTeamServiceOverrides {
        create_task: Some(Box::new(|_, _| Ok(fake_task_with(json!({ "subject": "line one\nline two" }))))),
        ..Default::default()
    });

    // when
    let result = run_team_task_create(
        &service,
        &create_input(json!({ "team_run_id": "run-1", "subject": "line one\nline two", "description": "d" })),
    )
    .unwrap();

    // then
    let text = result_text(&result_json(&result));
    assert!(text.contains("'line one line two'"), "{text}");
    assert_eq!(text.split('\n').count(), 1);
}

// ---- task_create tool ----

#[test]
fn given_a_new_task_when_create_runs_then_it_reports_the_created_task() {
    let service = create_fake_team_service(FakeTeamServiceOverrides {
        create_task: Some(Box::new(|_, _| Ok(fake_task()))),
        ..Default::default()
    });
    let result = run_team_task_create(
        &service,
        &create_input(json!({ "team_run_id": "run-1", "subject": "s", "description": "d" })),
    )
    .unwrap();
    let value = result_json(&result);
    assert_eq!(value["details"]["kind"], json!("created"));
    let text = result_text(&value);
    assert!(text.contains("Created task task-1"), "{text}");
    assert!(text.contains("'do the thing'"), "{text}");
    assert!(text.contains("pending"), "{text}");
    let calls = service.calls();
    assert_eq!(calls[0].method, "createTask");
    assert_eq!(calls[0].args[0], json!("run-1"));
    assert_eq!(calls[0].args[1]["subject"], json!("s"));
    assert_eq!(calls[0].args[1]["description"], json!("d"));
    assert_eq!(calls[0].args[1]["status"], json!("pending"));
}

#[test]
fn given_a_blocked_task_when_create_runs_then_the_text_names_the_blockers() {
    let service = create_fake_team_service(FakeTeamServiceOverrides {
        create_task: Some(Box::new(|_, _| Ok(fake_task_with(json!({ "blockedBy": ["task-0"] }))))),
        ..Default::default()
    });
    let result = run_team_task_create(
        &service,
        &create_input(json!({
            "team_run_id": "run-1",
            "subject": "s",
            "description": "d",
            "blocked_by": ["task-0"],
        })),
    )
    .unwrap();
    let text = result_text(&result_json(&result));
    assert!(text.contains("blocked by: task-0"), "{text}");
}

#[test]
fn given_a_subject_with_embedded_newlines_when_create_runs_then_the_text_collapses_them_onto_one_line() {
    assert_multiline_subject_collapsed();
}

#[test]
fn given_a_subject_with_embedded_newlines_when_create_runs_then_the_text_collapses_them_onto_one_line_duplicate() {
    assert_multiline_subject_collapsed();
}

#[test]
fn given_adversarial_owner_and_blocked_by_values_when_list_and_get_run_then_the_echoed_fields_are_collapsed_and_bounded()
{
    // given
    let hostile = format!("x\ninjected {}", "y".repeat(500));
    let task = fake_task_with(json!({ "owner": hostile, "blockedBy": [hostile], "blocks": [hostile] }));
    let list_task = task.clone();
    let get_task = task;
    let service = create_fake_team_service(FakeTeamServiceOverrides {
        list_tasks: Some(Box::new(move |_, _| Ok(vec![list_task.clone()]))),
        get_task: Some(Box::new(move |_, _| Ok(get_task.clone()))),
        ..Default::default()
    });

    // when
    let listed = run_team_task_list(&service, &list_input(json!({ "team_run_id": "run-1" }))).unwrap();
    let fetched =
        run_team_task_get(&service, &get_input(json!({ "team_run_id": "run-1", "task_id": "task-1" }))).unwrap();

    // then
    let list_text = result_text(&result_json(&listed));
    let get_text = result_text(&result_json(&fetched));
    for text in [&list_text, &get_text] {
        assert!(!text.contains("injected\n"), "{text}");
        assert!(!text.contains(&hostile), "{text}");
    }
    assert!(list_text.contains("owner:x injected"), "{list_text}");
}

#[test]
fn given_the_factory_when_built_then_it_names_the_tool_task_create() {
    assert_eq!(create_team_task_create_tool(&deps_with_default_service()).name, "task_create");
}

// ---- task_list tool ----

#[test]
fn given_tasks_when_list_runs_then_it_reports_them_forwarding_the_filter() {
    let service = create_fake_team_service(FakeTeamServiceOverrides {
        list_tasks: Some(Box::new(|_, _| {
            Ok(vec![
                fake_task(),
                fake_task_with(json!({ "id": "task-2", "status": "claimed", "owner": "alpha" })),
            ])
        })),
        ..Default::default()
    });
    let result =
        run_team_task_list(&service, &list_input(json!({ "team_run_id": "run-1", "status": "pending" }))).unwrap();
    let value = result_json(&result);
    assert_eq!(value["details"]["kind"], json!("list"));
    assert_eq!(value["details"]["tasks"].as_array().map(Vec::len), Some(2));
    let text = result_text(&value);
    let first_line = text.split('\n').next().unwrap_or("");
    assert_eq!(first_line, "2 task(s).");
    assert!(text.contains("- task-1 [pending] 'do the thing'"), "{text}");
    assert!(text.contains("- task-2 [claimed] owner:alpha 'do the thing'"), "{text}");
    let calls = service.calls();
    assert_eq!(calls[0].method, "listTasks");
    assert_eq!(calls[0].args[0], json!("run-1"));
    assert_eq!(calls[0].args[1]["status"], json!("pending"));
}

#[test]
fn given_the_factory_when_built_then_it_names_the_tool_task_list() {
    assert_eq!(create_team_task_list_tool(&deps_with_default_service()).name, "task_list");
}

// ---- task_get tool ----

#[test]
fn given_an_existing_task_when_get_runs_then_it_reports_the_task() {
    let service = create_fake_team_service(FakeTeamServiceOverrides {
        get_task: Some(Box::new(|_, _| Ok(fake_task_with(json!({ "owner": "alpha" }))))),
        ..Default::default()
    });
    let result =
        run_team_task_get(&service, &get_input(json!({ "team_run_id": "run-1", "task_id": "task-1" }))).unwrap();
    let value = result_json(&result);
    assert_eq!(value["details"]["kind"], json!("task"));
    let text = result_text(&value);
    assert!(text.contains("Task task-1: pending."), "{text}");
    assert!(text.contains("subject: do the thing"), "{text}");
    assert!(text.contains("owner: alpha"), "{text}");
    assert!(text.contains("description: details"), "{text}");
}

#[test]
fn given_a_missing_task_when_get_runs_then_it_reports_not_found() {
    let service = create_fake_team_service(FakeTeamServiceOverrides {
        get_task: Some(Box::new(|_, _| Err(named_error("ENOENT", "missing")))),
        ..Default::default()
    });
    let result =
        run_team_task_get(&service, &get_input(json!({ "team_run_id": "run-1", "task_id": "ghost" }))).unwrap();
    let value = result_json(&result);
    assert_eq!(value["details"]["kind"], json!("not_found"));
    assert_eq!(value["details"]["task_id"], json!("ghost"));
}

#[test]
fn given_the_factory_when_built_then_it_names_the_tool_task_get() {
    assert_eq!(create_team_task_get_tool(&deps_with_default_service()).name, "task_get");
}

// ---- task_update tool ----

#[test]
fn given_a_status_update_when_update_runs_then_it_reports_the_updated_task() {
    let service = create_fake_team_service(FakeTeamServiceOverrides {
        update_task: Some(Box::new(|_| Ok(fake_task_with(json!({ "status": "in_progress" }))))),
        ..Default::default()
    });
    let result = run_team_task_update(
        &service,
        &update_input(json!({ "team_run_id": "run-1", "task_id": "task-1", "status": "in_progress" })),
    )
    .unwrap();
    let value = result_json(&result);
    assert_eq!(value["details"]["kind"], json!("updated"));
    let text = result_text(&value);
    assert!(text.contains("Updated task task-1 to in_progress"), "{text}");
    assert!(text.contains("'do the thing'"), "{text}");
    let calls = service.calls();
    assert_eq!(calls[0].method, "updateTask");
    assert_eq!(calls[0].args[0]["teamRunId"], json!("run-1"));
    assert_eq!(calls[0].args[0]["taskId"], json!("task-1"));
    assert_eq!(calls[0].args[0]["status"], json!("in_progress"));
}

#[test]
fn given_an_already_claimed_task_when_claim_runs_then_it_reports_already_claimed() {
    let service = create_fake_team_service(FakeTeamServiceOverrides {
        update_task: Some(Box::new(|_| {
            Err(named_error("TeamTaskAlreadyClaimedError", "Task is already claimed"))
        })),
        ..Default::default()
    });
    let result = run_team_task_update(
        &service,
        &update_input(json!({ "team_run_id": "run-1", "task_id": "task-1", "status": "claimed", "owner": "alpha" })),
    )
    .unwrap();
    let value = result_json(&result);
    assert_eq!(value["details"]["kind"], json!("already_claimed"));
    assert_eq!(value["details"]["task_id"], json!("task-1"));
}

#[test]
fn given_a_blocked_task_when_claim_runs_then_it_reports_blocked_by() {
    let service = create_fake_team_service(FakeTeamServiceOverrides {
        update_task: Some(Box::new(|_| Err(named_error("TeamTaskBlockedByError", "Task is blocked by: task-0")))),
        ..Default::default()
    });
    let result = run_team_task_update(
        &service,
        &update_input(json!({ "team_run_id": "run-1", "task_id": "task-1", "status": "claimed" })),
    )
    .unwrap();
    assert_eq!(result.details.kind(), "blocked_by");
}

#[test]
fn given_an_illegal_transition_when_update_runs_then_it_reports_invalid_transition() {
    let service = create_fake_team_service(FakeTeamServiceOverrides {
        update_task: Some(Box::new(|_| {
            Err(named_error(
                "TeamTaskInvalidTransitionError",
                "Invalid task transition from completed to pending",
            ))
        })),
        ..Default::default()
    });
    let result = run_team_task_update(
        &service,
        &update_input(json!({ "team_run_id": "run-1", "task_id": "task-1", "status": "pending" })),
    )
    .unwrap();
    assert_eq!(result.details.kind(), "invalid_transition");
}

#[test]
fn given_a_cross_owner_update_when_update_runs_then_it_reports_cross_owner() {
    let service = create_fake_team_service(FakeTeamServiceOverrides {
        update_task: Some(Box::new(|_| {
            Err(named_error("TeamTaskCrossOwnerUpdateError", "Cannot update a task owned by another member"))
        })),
        ..Default::default()
    });
    let result = run_team_task_update(
        &service,
        &update_input(json!({ "team_run_id": "run-1", "task_id": "task-1", "status": "completed", "owner": "alpha" })),
    )
    .unwrap();
    assert_eq!(result.details.kind(), "cross_owner");
}

#[test]
fn given_the_factory_when_built_then_it_names_the_tool_task_update() {
    assert_eq!(create_team_task_update_tool(&deps_with_default_service()).name, "task_update");
}
