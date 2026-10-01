//! `tools/task/argument-normalization.test.ts`

use pretty_assertions::assert_eq;
use serde_json::{Value, json};

use crate::tools::task::argument_normalization::normalize_task_tool_arguments;
use crate::tools::task::types::SpawnTarget;
use crate::tools::task::validation::{SpawnItemInput, SpawnParamsInput, resolve_spawn_items};

/// `tool.prepareArguments` delegates straight to `normalize_task_tool_arguments`.
fn prepare_arguments(raw: &Value) -> Value {
    normalize_task_tool_arguments(raw)
}

fn str_field(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_string)
}

fn list_field(value: &Value, key: &str) -> Option<Vec<String>> {
    value.get(key).and_then(Value::as_array).map(|entries| {
        entries
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect()
    })
}

fn to_item(value: &Value) -> SpawnItemInput {
    SpawnItemInput {
        prompt: str_field(value, "prompt").unwrap_or_default(),
        category: str_field(value, "category"),
        subagent_type: str_field(value, "subagent_type"),
        model: str_field(value, "model"),
        task_summary: str_field(value, "task_summary"),
        description: str_field(value, "description"),
        name: str_field(value, "name"),
        load_skills: list_field(value, "load_skills"),
    }
}

/// Maps the normalized JSON params onto the typed spawn params, as the TS passes them through.
fn to_params(value: &Value) -> SpawnParamsInput {
    SpawnParamsInput {
        prompt: str_field(value, "prompt"),
        category: str_field(value, "category"),
        subagent_type: str_field(value, "subagent_type"),
        model: str_field(value, "model"),
        task_summary: str_field(value, "task_summary"),
        description: str_field(value, "description"),
        name: str_field(value, "name"),
        load_skills: list_field(value, "load_skills"),
        run_in_background: value.get("run_in_background").and_then(Value::as_bool),
        tasks: value
            .get("tasks")
            .and_then(Value::as_array)
            .map(|tasks| tasks.iter().map(to_item).collect()),
    }
}

#[test]
fn given_a_gpt_padded_single_spawn_when_arguments_are_prepared_then_serializer_only_fields_are_removed()
 {
    // when
    let prepared = prepare_arguments(&json!({
        "prompt": "TASK: Audit the task tool boundary.",
        "description": "Audit task tool",
        "category": "",
        "subagent_type": "explore",
        "run_in_background": true,
        "name": "task-padding-audit",
        "model": "",
        "load_skills": [],
        "tasks": [
            {
                "prompt": "unused",
                "description": "unused",
                "category": "quick",
                "subagent_type": "",
                "name": "unused",
                "model": "",
                "load_skills": [],
            },
        ],
    }));

    // then
    assert_eq!(
        prepared,
        json!({
            "prompt": "TASK: Audit the task tool boundary.",
            "description": "Audit task tool",
            "subagent_type": "explore",
            "run_in_background": true,
            "name": "task-padding-audit",
        })
    );
}

#[test]
fn given_a_gpt_padded_batch_when_arguments_are_prepared_then_blank_top_level_prompt_and_item_overrides_do_not_block_fan_out()
 {
    // when
    let prepared = prepare_arguments(&json!({
        "prompt": "",
        "description": "",
        "category": "",
        "subagent_type": "",
        "run_in_background": true,
        "name": "",
        "model": "",
        "load_skills": ["audit"],
        "tasks": [
            {
                "prompt": "TASK: Inspect the argument schema.",
                "description": "",
                "category": "",
                "subagent_type": "explore",
                "name": "",
                "model": "",
                "load_skills": [],
            },
            {
                "prompt": "TASK: Inspect the prompt surfaces.",
                "description": "",
                "category": "quick",
                "subagent_type": "",
                "name": "",
                "model": "",
                "load_skills": [],
            },
        ],
    }));
    let resolved = resolve_spawn_items(&to_params(&prepared));

    // then
    let items = resolved.expect("expected ok resolution");
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].prompt, "TASK: Inspect the argument schema.");
    assert_eq!(
        items[0].target,
        SpawnTarget::SubagentType("explore".to_string())
    );
    assert_eq!(items[0].load_skills, vec!["audit".to_string()]);
    assert_eq!(items[1].prompt, "TASK: Inspect the prompt surfaces.");
    assert_eq!(items[1].target, SpawnTarget::Category("quick".to_string()));
    assert_eq!(items[1].load_skills, vec!["audit".to_string()]);
}

#[test]
fn given_an_over_limit_task_summary_when_arguments_are_prepared_then_the_harness_force_truncates_it_to_the_schema_limit()
 {
    // when
    let prepared = prepare_arguments(&json!({
        "prompt": "TASK: Audit the task tool boundary.",
        "task_summary": format!("Audit {}", "z".repeat(200)),
        "subagent_type": "explore",
    }));

    // then
    let summary = prepared
        .get("task_summary")
        .and_then(Value::as_str)
        .expect("task_summary present");
    assert_eq!(summary.chars().count(), 80);
    assert!(summary.ends_with("..."));
}

#[test]
fn given_blank_and_item_task_summary_values_when_arguments_are_prepared_then_blanks_drop_and_item_summaries_are_clamped()
 {
    // when
    let prepared = prepare_arguments(&json!({
        "task_summary": "   ",
        "category": "quick",
        "tasks": [
            { "prompt": "TASK: Inspect the schema.", "task_summary": "  Inspect the\n   schema surface  " },
            { "prompt": "TASK: Inspect the prompts.", "task_summary": "w".repeat(120) },
        ],
    }));

    // then
    assert!(prepared.get("task_summary").is_none());
    let tasks = prepared
        .get("tasks")
        .and_then(Value::as_array)
        .expect("tasks present");
    assert_eq!(
        tasks[0].get("task_summary").and_then(Value::as_str),
        Some("Inspect the schema surface")
    );
    let second = tasks[1]
        .get("task_summary")
        .and_then(Value::as_str)
        .expect("second summary present");
    assert_eq!(second.chars().count(), 80);
    assert!(second.ends_with("..."));
}

#[test]
fn given_genuinely_meaningful_prompt_and_tasks_when_arguments_are_prepared_then_semantic_ambiguity_remains_a_hard_error()
 {
    // when
    let prepared = prepare_arguments(&json!({
        "prompt": "TASK: Run the single assignment.",
        "subagent_type": "explore",
        "tasks": [{ "prompt": "TASK: Run the separate batch assignment.", "category": "quick" }],
    }));
    let resolved = resolve_spawn_items(&to_params(&prepared));

    // then
    let error = resolved.expect_err("expected meaningful prompt/tasks ambiguity to fail");
    assert_eq!(error.code(), "prompt_and_tasks");
}
