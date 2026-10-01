//! `tools/task/execute-batch-background.test.ts`

use std::collections::VecDeque;
use std::sync::Mutex;

use pretty_assertions::assert_eq;

use crate::manager::execution_mode::ExecutionMode;
use crate::manager::manager_tests::fakes::default_manager;
use crate::manager::types::{StartFailure, StartResult, StartedTask};
use crate::state::TaskStatus;
use crate::tools::task::execute_batch::{ExecuteBatchInput, TaskToolResult, execute_batch};
use crate::tools::task::foreground_wait::{ForegroundWaitOptions, TaskToolContext};
use crate::tools::task::types::{ResolvedSpawnItem, TaskSkillSummary};
use crate::tools::task::validation::{SpawnItemInput, SpawnParamsInput, resolve_spawn_items};

const IDS: [&str; 3] = ["st_batch_1", "st_batch_2", "st_batch_3"];

type StartItemFn = dyn Fn(&ResolvedSpawnItem) -> Result<StartResult, String> + Send + Sync;
type SkillSummaryFn = dyn Fn(&ResolvedSpawnItem) -> Option<TaskSkillSummary> + Send + Sync;

fn ctx() -> TaskToolContext {
    TaskToolContext {
        session_id: "parent-1".into(),
        cwd: "/tmp".into(),
        ..Default::default()
    }
}

fn text_of(result: &TaskToolResult) -> String {
    match result.content.first() {
        Some(content) if content.kind == "text" => content.text.clone(),
        _ => String::new(),
    }
}

fn started(task_id: &str, name: &str, status: TaskStatus, queue_position: Option<usize>) -> StartResult {
    StartResult::Started(StartedTask {
        task_id: task_id.to_string(),
        status,
        name: name.to_string(),
        queue_position,
        name_warning: None,
        resolved_model: None,
    })
}

fn start_failed(task_id: &str, name: &str, message: &str) -> StartResult {
    StartResult::StartFailed(StartFailure {
        task_id: task_id.to_string(),
        name: name.to_string(),
        category: Some("quick".to_string()),
        execution_mode: ExecutionMode::InProcess,
        model: "test/model".to_string(),
        run_in_background: true,
        error_message: message.to_string(),
        resolved_model: None,
        subagent_type: None,
    })
}

/// The TS `createFakeManager({ start })`: returns scripted start results in order.
fn scripted_starts(results: Vec<StartResult>) -> &'static StartItemFn {
    let queue = Mutex::new(VecDeque::from(results));
    Box::leak(Box::new(move |_item: &ResolvedSpawnItem| {
        queue
            .lock()
            .expect("queue lock")
            .pop_front()
            .ok_or_else(|| "unexpected extra start".to_string())
    }))
}

fn item(prompt: &str, load_skills: Option<Vec<&str>>) -> SpawnItemInput {
    SpawnItemInput {
        prompt: prompt.to_string(),
        load_skills: load_skills.map(|names| names.into_iter().map(str::to_string).collect()),
        ..Default::default()
    }
}

fn background_params(tasks: Vec<SpawnItemInput>, load_skills: Option<Vec<&str>>) -> SpawnParamsInput {
    SpawnParamsInput {
        category: Some("quick".to_string()),
        run_in_background: Some(true),
        load_skills: load_skills.map(|names| names.into_iter().map(str::to_string).collect()),
        tasks: Some(tasks),
        ..Default::default()
    }
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_string()).collect()
}

fn run_batch(
    params: &SpawnParamsInput,
    start_item: &'static StartItemFn,
    skill_summary_for: Option<&'static SkillSummaryFn>,
) -> TaskToolResult {
    let harness = default_manager();
    let items = resolve_spawn_items(params).expect("items resolve");
    let context = ctx();
    execute_batch(&ExecuteBatchInput {
        manager: &harness.manager,
        items: &items,
        signal: None,
        ctx: &context,
        run_in_background: true,
        start_item,
        skill_summary_for,
        options: ForegroundWaitOptions::default(),
    })
}

#[test]
fn given_background_capacity_one_when_three_items_start_then_all_ids_and_queue_positions_return_as_running() {
    let start_item = scripted_starts(vec![
        started(IDS[0], "one", TaskStatus::Running, None),
        started(IDS[1], "two", TaskStatus::Pending, Some(1)),
        started(IDS[2], "three", TaskStatus::Pending, Some(2)),
    ]);
    let params = background_params(
        vec![item("one", None), item("two", None), item("three", None)],
        None,
    );

    let output = run_batch(&params, start_item, None);

    assert_eq!(output.details.task_id, IDS[0]);
    assert_eq!(output.details.status, "running");
    assert_eq!(output.details.run_in_background, Some(true));
    let items = output.details.items.clone().expect("items present");
    assert_eq!(items.len(), 3);
    let expected = [
        (IDS[0], "one", "running", None),
        (IDS[1], "two", "pending", Some(1)),
        (IDS[2], "three", "pending", Some(2)),
    ];
    for (actual, (task_id, name, status, queue_position)) in items.iter().zip(expected) {
        assert_eq!(actual.task_id, task_id);
        assert_eq!(actual.name.as_deref(), Some(name));
        assert_eq!(actual.category.as_deref(), Some("quick"));
        assert_eq!(actual.status, status);
        assert_eq!(actual.queue_position, queue_position);
    }
    let text = text_of(&output);
    for task_id in IDS {
        assert!(
            text.contains(&format!("task_send(to=\"{task_id}\"")),
            "missing task_send hint for {task_id} in {text}"
        );
    }
}

#[test]
fn given_inherited_and_item_load_skills_when_background_items_start_then_each_result_reports_its_own_resolution() {
    let start_item = scripted_starts(vec![
        started(IDS[0], "item-1", TaskStatus::Running, None),
        started(IDS[1], "item-2", TaskStatus::Running, None),
    ]);
    let skill_summary_for: &'static SkillSummaryFn =
        Box::leak(Box::new(|item: &ResolvedSpawnItem| match item.prompt.as_str() {
            "one" => Some(TaskSkillSummary {
                requested: strings(&["shared"]),
                resolved: strings(&["shared"]),
                missing: Vec::new(),
            }),
            "two" => Some(TaskSkillSummary {
                requested: strings(&["specific", "ghost"]),
                resolved: strings(&["specific"]),
                missing: strings(&["ghost"]),
            }),
            _ => None,
        }));
    let params = background_params(
        vec![item("one", None), item("two", Some(vec!["specific", "ghost"]))],
        Some(vec!["shared"]),
    );

    let output = run_batch(&params, start_item, Some(skill_summary_for));

    let items = output.details.items.clone().expect("items present");
    let first = items[0].skills.clone().expect("first skills");
    assert_eq!(first.requested, strings(&["shared"]));
    assert_eq!(first.resolved, strings(&["shared"]));
    assert_eq!(first.missing, Vec::<String>::new());
    let second = items[1].skills.clone().expect("second skills");
    assert_eq!(second.requested, strings(&["specific", "ghost"]));
    assert_eq!(second.resolved, strings(&["specific"]));
    assert_eq!(second.missing, strings(&["ghost"]));
    assert!(text_of(&output).contains("Missing skills: ghost"));
}

#[test]
fn given_every_background_start_fails_when_results_are_aggregated_then_status_is_error_instead_of_running() {
    let start_item = scripted_starts(
        IDS.iter()
            .enumerate()
            .map(|(index, task_id)| {
                start_failed(task_id, &format!("item-{}", index + 1), &format!("failed:{task_id}"))
            })
            .collect(),
    );
    let params = background_params(
        vec![item("one", None), item("two", None), item("three", None)],
        None,
    );

    let output = run_batch(&params, start_item, None);

    assert_eq!(output.details.task_id, "");
    assert_eq!(output.details.status, "error");
    let messages: Vec<Option<String>> = output
        .details
        .items
        .clone()
        .expect("items present")
        .into_iter()
        .map(|item| item.error_message)
        .collect();
    assert_eq!(
        messages,
        IDS.iter()
            .map(|task_id| Some(format!("failed:{task_id}")))
            .collect::<Vec<_>>()
    );
}
