//! `tools/task/validation.test.ts`.
// allow: SIZE_OK - one-to-one translation of validation.test.ts.

use pretty_assertions::assert_eq;

use super::*;
use crate::tools::task::types::{TaskToolDetails, TaskToolItemDetail, TaskToolMode};

fn target(
    category: Option<&str>,
    subagent: Option<&str>,
    model: Option<&str>,
) -> TaskTargetSelection {
    validate_task_target(TargetInput {
        category,
        subagent_type: subagent,
        model,
    })
}

fn target_error_of(selection: TaskTargetSelection) -> TaskTargetError {
    match selection {
        TaskTargetSelection::Error(error) => error,
        other => panic!("expected error, got {other:?}"),
    }
}

fn item(prompt: &str) -> SpawnItemInput {
    SpawnItemInput {
        prompt: prompt.to_string(),
        ..SpawnItemInput::default()
    }
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(ToString::to_string).collect()
}

fn resolve_err(params: &SpawnParamsInput) -> ResolveSpawnItemsError {
    resolve_spawn_items(params).expect_err("expected error")
}

fn resolve_ok(params: &SpawnParamsInput) -> Vec<ResolvedSpawnItem> {
    resolve_spawn_items(params).expect("expected ok")
}

fn shape_code(params: &SpawnParamsInput) -> BatchShapeErrorCode {
    match validate_batch_shape(params) {
        BatchShape::Error(error) => error.code,
        other => panic!("expected shape error, got {other:?}"),
    }
}

// validateTaskTarget

#[test]
fn given_only_category_when_validated_then_resolves_to_a_category_selection() {
    assert_eq!(
        target(Some("quick"), None, None),
        TaskTargetSelection::Category("quick".to_string())
    );
}

#[test]
fn given_only_subagent_type_when_validated_then_resolves_to_a_subagent_selection() {
    assert_eq!(
        target(None, Some("momus"), None),
        TaskTargetSelection::SubagentType("momus".to_string())
    );
}

#[test]
fn given_both_category_and_subagent_type_when_validated_then_returns_a_typed_both_targets_error() {
    let error = target_error_of(target(Some("quick"), Some("momus"), None));

    assert_eq!(error.code, TaskTargetErrorCode::BothTargets);
    assert!(error.message.contains("EITHER category OR subagent_type"));
    assert!(error.message.contains("not both"));
    assert!(!error.message.contains("ignored"));
    assert!(error.message.contains("Remove one and retry"));
}

#[test]
fn given_neither_category_nor_subagent_type_when_validated_then_returns_a_typed_no_target_error() {
    let error = target_error_of(target(None, None, None));

    assert_eq!(error.code, TaskTargetErrorCode::NoTarget);
    assert!(
        error
            .message
            .contains("MUST provide EITHER category OR subagent_type")
    );
}

#[test]
fn given_empty_string_category_when_validated_then_treated_as_absent() {
    let error = target_error_of(target(Some("  "), None, None));

    assert_eq!(error.code, TaskTargetErrorCode::NoTarget);
}

// validateBatchShape

#[test]
fn given_prompt_without_tasks_when_shape_validated_then_resolves_to_single() {
    let params = SpawnParamsInput {
        prompt: Some("do it".to_string()),
        category: Some("quick".to_string()),
        ..SpawnParamsInput::default()
    };

    assert_eq!(validate_batch_shape(&params), BatchShape::Single);
}

#[test]
fn given_tasks_without_prompt_when_shape_validated_then_resolves_to_batch() {
    let params = SpawnParamsInput {
        tasks: Some(vec![SpawnItemInput {
            category: Some("quick".to_string()),
            ..item("a")
        }]),
        ..SpawnParamsInput::default()
    };

    assert_eq!(validate_batch_shape(&params), BatchShape::Batch);
}

fn prompt_and_tasks() -> SpawnParamsInput {
    SpawnParamsInput {
        prompt: Some("do it".to_string()),
        tasks: Some(vec![SpawnItemInput {
            category: Some("quick".to_string()),
            ..item("a")
        }]),
        ..SpawnParamsInput::default()
    }
}

#[test]
fn given_both_prompt_and_tasks_when_shape_validated_then_names_both_fields_in_a_typed_error() {
    let BatchShape::Error(error) = validate_batch_shape(&prompt_and_tasks()) else {
        panic!("expected error");
    };

    assert_eq!(error.code, BatchShapeErrorCode::PromptAndTasks);
    assert!(error.message.contains("prompt"));
    assert!(error.message.contains("tasks"));
}

#[test]
fn given_neither_prompt_nor_tasks_when_shape_validated_then_returns_a_no_prompt_or_tasks_error() {
    let params = SpawnParamsInput {
        category: Some("quick".to_string()),
        ..SpawnParamsInput::default()
    };

    assert_eq!(shape_code(&params), BatchShapeErrorCode::NoPromptOrTasks);
}

#[test]
fn given_an_empty_tasks_array_when_shape_validated_then_returns_an_empty_tasks_error() {
    let params = SpawnParamsInput {
        tasks: Some(Vec::new()),
        ..SpawnParamsInput::default()
    };

    assert_eq!(shape_code(&params), BatchShapeErrorCode::EmptyTasks);
}

// resolveSpawnItems

#[test]
fn given_legacy_single_prompt_params_when_resolved_then_yields_exactly_one_item() {
    let params = SpawnParamsInput {
        prompt: Some("do it".to_string()),
        subagent_type: Some("momus".to_string()),
        model: Some("anthropic/claude-opus-4".to_string()),
        load_skills: Some(strings(&["a"])),
        ..SpawnParamsInput::default()
    };

    let items = resolve_ok(&params);

    assert_eq!(
        items,
        vec![ResolvedSpawnItem {
            prompt: "do it".to_string(),
            task_summary: None,
            description: None,
            name: None,
            model: Some("anthropic/claude-opus-4".to_string()),
            load_skills: strings(&["a"]),
            target: SpawnTarget::SubagentType("momus".to_string()),
        }]
    );
}

#[test]
fn given_a_three_item_batch_when_resolved_then_inherits_top_level_model_and_subagent_and_item_overrides_win()
 {
    let params = SpawnParamsInput {
        subagent_type: Some("momus".to_string()),
        model: Some("anthropic/claude-opus-4".to_string()),
        load_skills: Some(strings(&["shared"])),
        tasks: Some(vec![
            item("one"),
            SpawnItemInput {
                model: Some("anthropic/claude-haiku".to_string()),
                ..item("two")
            },
            SpawnItemInput {
                load_skills: Some(strings(&["extra"])),
                ..item("three")
            },
        ]),
        ..SpawnParamsInput::default()
    };

    let items = resolve_ok(&params);

    let facts: Vec<(SpawnTarget, Option<&str>, Vec<String>)> = items
        .iter()
        .map(|item| {
            (
                item.target.clone(),
                item.model.as_deref(),
                item.load_skills.clone(),
            )
        })
        .collect();
    let momus = SpawnTarget::SubagentType("momus".to_string());
    assert_eq!(
        facts,
        vec![
            (
                momus.clone(),
                Some("anthropic/claude-opus-4"),
                strings(&["shared"])
            ),
            (
                momus.clone(),
                Some("anthropic/claude-haiku"),
                strings(&["shared"])
            ),
            (momus, Some("anthropic/claude-opus-4"), strings(&["extra"])),
        ]
    );
}

#[test]
fn given_an_item_subagent_type_when_resolved_then_it_suppresses_the_inherited_top_level_category() {
    let params = SpawnParamsInput {
        category: Some("quick".to_string()),
        tasks: Some(vec![
            item("one"),
            SpawnItemInput {
                subagent_type: Some("momus".to_string()),
                ..item("two")
            },
        ]),
        ..SpawnParamsInput::default()
    };

    let targets: Vec<SpawnTarget> = resolve_ok(&params)
        .into_iter()
        .map(|item| item.target)
        .collect();

    assert_eq!(
        targets,
        vec![
            SpawnTarget::Category("quick".to_string()),
            SpawnTarget::SubagentType("momus".to_string()),
        ]
    );
}

#[test]
fn given_both_prompt_and_tasks_when_resolved_then_returns_a_typed_error_naming_both_fields() {
    let error = resolve_err(&prompt_and_tasks());

    assert!(error.message().contains("prompt"));
    assert!(error.message().contains("tasks"));
}

#[test]
fn given_neither_prompt_nor_tasks_when_resolved_then_returns_a_typed_error() {
    let params = SpawnParamsInput {
        category: Some("quick".to_string()),
        ..SpawnParamsInput::default()
    };

    assert_eq!(resolve_err(&params).code(), "no_prompt_or_tasks");
}

#[test]
fn given_an_empty_tasks_array_when_resolved_then_returns_a_typed_error() {
    let params = SpawnParamsInput {
        tasks: Some(Vec::new()),
        ..SpawnParamsInput::default()
    };

    assert_eq!(resolve_err(&params).code(), "empty_tasks");
}

#[test]
fn given_an_item_with_both_category_and_subagent_type_when_resolved_then_returns_an_item_target_error_naming_the_index()
 {
    let params = SpawnParamsInput {
        category: Some("quick".to_string()),
        tasks: Some(vec![
            item("ok"),
            SpawnItemInput {
                category: Some("deep".to_string()),
                subagent_type: Some("momus".to_string()),
                ..item("bad")
            },
        ]),
        ..SpawnParamsInput::default()
    };

    let ResolveSpawnItemsError::ItemTarget { index, message } = resolve_err(&params) else {
        panic!("expected item_target error");
    };
    assert_eq!(index, 1);
    assert!(message.contains('1'));
}

// resolveSpawnItems task_summary

#[test]
fn given_a_single_spawn_with_a_task_summary_when_resolved_then_the_item_carries_the_summary() {
    let params = SpawnParamsInput {
        prompt: Some("TASK: Audit the boundary.".to_string()),
        task_summary: Some("Audit the boundary".to_string()),
        category: Some("quick".to_string()),
        ..SpawnParamsInput::default()
    };

    let items = resolve_ok(&params);

    assert_eq!(items[0].task_summary.as_deref(), Some("Audit the boundary"));
}

#[test]
fn given_batch_items_with_and_without_task_summary_when_resolved_then_summaries_stay_per_item_and_never_inherit()
 {
    let params = SpawnParamsInput {
        task_summary: Some("Top-level summary".to_string()),
        category: Some("quick".to_string()),
        tasks: Some(vec![
            SpawnItemInput {
                task_summary: Some("First summary".to_string()),
                ..item("TASK: first.")
            },
            item("TASK: second."),
        ]),
        ..SpawnParamsInput::default()
    };

    let summaries: Vec<Option<String>> = resolve_ok(&params)
        .into_iter()
        .map(|item| item.task_summary)
        .collect();

    assert_eq!(summaries, vec![Some("First summary".to_string()), None]);
}

// batch spawn types

#[test]
fn given_the_batch_types_when_constructed_then_resolved_item_and_item_detail_have_the_documented_shape()
 {
    let category_item = ResolvedSpawnItem {
        prompt: "p".to_string(),
        task_summary: None,
        description: None,
        name: None,
        model: None,
        load_skills: Vec::new(),
        target: SpawnTarget::Category("quick".to_string()),
    };
    let detail = TaskToolItemDetail {
        task_id: "t1".to_string(),
        status: "completed".to_string(),
        ..TaskToolItemDetail::default()
    };

    assert_eq!(
        category_item.target,
        SpawnTarget::Category("quick".to_string())
    );
    assert_eq!(
        serde_json::to_value(&detail).expect("json"),
        serde_json::json!({ "task_id": "t1", "status": "completed" })
    );
}

#[test]
fn given_task_tool_details_when_constructed_with_items_then_the_additive_items_field_is_accepted() {
    let details = TaskToolDetails {
        task_id: "t1".to_string(),
        status: "completed".to_string(),
        mode: TaskToolMode::Spawn,
        items: Some(vec![TaskToolItemDetail {
            task_id: "c1".to_string(),
            status: "completed".to_string(),
            ..TaskToolItemDetail::default()
        }]),
        ..TaskToolDetails::default()
    };

    let json = serde_json::to_value(&details).expect("json");

    assert_eq!(json["mode"], "spawn");
    assert_eq!(json["items"][0]["task_id"], "c1");
}

// validateTaskTarget category+model exclusivity

#[test]
fn given_category_with_model_when_validated_then_returns_a_typed_category_with_model_error() {
    let error = target_error_of(target(
        Some("architect"),
        None,
        Some("quotio-openai/gpt-5.6-luna-fast"),
    ));

    assert_eq!(error.code, TaskTargetErrorCode::CategoryWithModel);
    assert!(error.message.contains("omo.json"));
}

#[test]
fn given_subagent_type_with_model_when_validated_then_resolves_to_a_subagent_selection() {
    assert_eq!(
        target(None, Some("momus"), Some("openai/gpt-5.6-sol")),
        TaskTargetSelection::SubagentType("momus".to_string())
    );
}

// resolveSpawnItems category+model exclusivity

#[test]
fn given_single_form_category_with_a_model_override_then_returns_an_item_target_error() {
    let params = SpawnParamsInput {
        prompt: Some("p".to_string()),
        category: Some("quick".to_string()),
        model: Some("openai/gpt-5.6-luna-fast".to_string()),
        ..SpawnParamsInput::default()
    };

    let error = resolve_err(&params);

    assert_eq!(error.code(), "item_target");
    assert!(error.message().contains("omo.json"));
}

#[test]
fn given_a_top_level_model_inherited_by_a_category_item_then_returns_an_item_target_error() {
    let params = SpawnParamsInput {
        model: Some("openai/gpt-5.6-luna-fast".to_string()),
        tasks: Some(vec![SpawnItemInput {
            category: Some("quick".to_string()),
            ..item("one")
        }]),
        ..SpawnParamsInput::default()
    };

    let error = resolve_err(&params);

    assert_eq!(error.code(), "item_target");
    assert!(error.message().contains("Task item 0"));
}

#[test]
fn given_a_top_level_category_and_an_item_model_then_returns_an_item_target_error() {
    let params = SpawnParamsInput {
        category: Some("quick".to_string()),
        tasks: Some(vec![
            item("one"),
            SpawnItemInput {
                model: Some("openai/gpt-5.6-luna-fast".to_string()),
                ..item("two")
            },
        ]),
        ..SpawnParamsInput::default()
    };

    let error = resolve_err(&params);

    assert_eq!(error.code(), "item_target");
    assert!(error.message().contains("Task item 1"));
}

#[test]
fn given_subagent_items_inheriting_a_top_level_model_then_resolves_ok_with_the_model_attached() {
    let params = SpawnParamsInput {
        subagent_type: Some("momus".to_string()),
        model: Some("openai/gpt-5.6-sol".to_string()),
        tasks: Some(vec![item("one")]),
        ..SpawnParamsInput::default()
    };

    let items = resolve_ok(&params);

    assert_eq!(items[0].model.as_deref(), Some("openai/gpt-5.6-sol"));
}
