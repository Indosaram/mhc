//! `tools/task/argument-normalization.test.ts`

use pretty_assertions::assert_eq;
use serde_json::{Value, json};

use crate::tools::task::argument_normalization::normalize_task_tool_arguments;
use crate::state::IsolationMergeMode;
use crate::tools::task::types::SpawnTarget;
use crate::tools::task::validation::{SpawnItemInput, SpawnParamsInput, resolve_spawn_items};

/// `tool.prepareArguments` delegates straight to `normalize_task_tool_arguments`.
fn prepare_arguments(raw: &Value) -> Value {
    normalize_task_tool_arguments(raw).expect("normalization succeeds")
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

/// The regression input conversion: a normalized `"merge"` string becomes the typed merge mode.
fn merge_field(value: &Value) -> Option<IsolationMergeMode> {
    match value.get("merge").and_then(Value::as_str) {
        Some("patch") => Some(IsolationMergeMode::Patch),
        Some("branch") => Some(IsolationMergeMode::Branch),
        _ => None,
    }
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
        isolated: value.get("isolated").and_then(Value::as_bool),
        apply: value.get("apply").and_then(Value::as_bool),
        merge: merge_field(value),
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
        isolated: value.get("isolated").and_then(Value::as_bool),
        apply: value.get("apply").and_then(Value::as_bool),
        merge: merge_field(value),
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

/// `isolationArguments` (TS 35-46) THROWS on a wrong-typed flag instead of dropping it; a dropped
/// flag would read as the config default at the manager boundary.
#[test]
fn given_a_wrong_typed_isolation_flag_when_arguments_are_prepared_then_normalization_rejects_it() {
    let error = normalize_task_tool_arguments(&json!({
        "prompt": "Inspect",
        "category": "quick",
        "isolated": "yes",
    }))
    .expect_err("a string isolated must be rejected");
    assert_eq!(error.message, "isolated must be a boolean");

    let error = normalize_task_tool_arguments(&json!({
        "category": "quick",
        "tasks": [{ "prompt": "Inspect", "apply": "no" }],
    }))
    .expect_err("a string item apply must be rejected");
    assert_eq!(error.message, "apply must be a boolean");

    let error = normalize_task_tool_arguments(&json!({
        "prompt": "Inspect",
        "category": "quick",
        "merge": "squash",
    }))
    .expect_err("an unknown merge must be rejected");
    assert_eq!(error.message, "merge must be patch or branch");
}

/// The batch case carries NO top-level prompt: a real prompt beside tasks is a genuine
/// prompt/tasks ambiguity (its own case above), so the batch-only shape is the one that resolves.
#[test]
fn given_batch_isolation_options_when_arguments_are_prepared_then_the_batch_only_shape_resolves() {
    // when
    let prepared = prepare_arguments(&json!({
        "isolated": true,
        "apply": false,
        "merge": "branch",
        "category": "quick",
        "tasks": [
            { "prompt": "First" },
            { "prompt": "Second", "isolated": false, "apply": true, "merge": "patch" },
        ],
    }));

    // then: the options survive normalization at the top level and per item.
    assert_eq!(prepared.get("isolated"), Some(&json!(true)));
    assert_eq!(prepared.get("apply"), Some(&json!(false)));
    assert_eq!(prepared.get("merge"), Some(&json!("branch")));
    let tasks = prepared
        .get("tasks")
        .and_then(Value::as_array)
        .expect("tasks present");
    assert_eq!(tasks[0].get("isolated"), None);
    assert_eq!(tasks[1].get("isolated"), Some(&json!(false)));
    assert_eq!(tasks[1].get("apply"), Some(&json!(true)));
    assert_eq!(tasks[1].get("merge"), Some(&json!("patch")));

    let items = resolve_spawn_items(&to_params(&prepared)).expect("the batch-only shape resolves");
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].prompt, "First");
    assert_eq!(items[1].prompt, "Second");
    assert_eq!(items[0].isolated, Some(true));
    assert_eq!(items[0].apply, Some(false));
    assert_eq!(items[0].merge, Some(IsolationMergeMode::Branch));
    assert_eq!(items[1].isolated, Some(false));
    assert_eq!(items[1].apply, Some(true));
    assert_eq!(items[1].merge, Some(IsolationMergeMode::Patch));
}

#[test]
fn given_a_single_spawn_with_isolation_options_when_arguments_are_prepared_then_it_resolves() {
    // when
    let prepared = prepare_arguments(&json!({
        "prompt": "Inspect",
        "isolated": true,
        "apply": true,
        "merge": "patch",
        "category": "quick",
    }));

    // then
    assert_eq!(prepared.get("isolated"), Some(&json!(true)));
    assert_eq!(prepared.get("apply"), Some(&json!(true)));
    assert_eq!(prepared.get("merge"), Some(&json!("patch")));

    let items = resolve_spawn_items(&to_params(&prepared)).expect("the single shape resolves");
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].prompt, "Inspect");
    assert_eq!(items[0].target, SpawnTarget::Category("quick".to_string()));
}

#[test]
fn batch_background_flags_and_kernel_tools_survive_normalization() {
    let raw = json!({
        "category": "quick",
        "run_in_background": true,
        "tools": ["inspect", "render"],
        "tasks": [
            { "prompt": "First", "run_in_background": false },
            { "prompt": "Second", "run_in_background": true },
        ],
    });

    let prepared = prepare_arguments(&raw);

    assert_eq!(prepared["tools"], json!(["inspect", "render"]));
    assert_eq!(prepared["run_in_background"], json!(true));
    assert_eq!(prepared["tasks"][0]["run_in_background"], json!(false));
    assert_eq!(prepared["tasks"][1]["run_in_background"], json!(true));
}
