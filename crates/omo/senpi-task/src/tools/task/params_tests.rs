//! `tools/task/params.test.ts`

use pretty_assertions::assert_eq;
use serde_json::{Value, json};

use crate::task_summary::TASK_SUMMARY_MAX_LENGTH;
use crate::tools::task::params::{MAX_TASK_BATCH_ITEMS, TASK_TOOL_PARAMS};

fn property_keys(schema: &Value) -> Vec<String> {
    schema["properties"]
        .as_object()
        .expect("properties object")
        .keys()
        .cloned()
        .collect()
}

fn index_of(keys: &[String], key: &str) -> usize {
    keys.iter()
        .position(|candidate| candidate == key)
        .unwrap_or_else(|| panic!("{key} missing"))
}

#[test]
fn given_schema_when_inspected_then_it_is_an_object_with_task_tool_fields() {
    assert_eq!(TASK_TOOL_PARAMS["type"], json!("object"));
    let keys = property_keys(&TASK_TOOL_PARAMS);
    for expected in [
        "prompt",
        "description",
        "category",
        "subagent_type",
        "run_in_background",
        "name",
        "model",
        "load_skills",
    ] {
        assert!(keys.iter().any(|key| key == expected), "missing {expected}");
    }
}

#[test]
fn given_schema_when_properties_inspected_then_removed_task_params_are_absent() {
    let keys = property_keys(&TASK_TOOL_PARAMS);
    assert!(keys.iter().any(|key| key == "prompt"));
    assert!(!keys.iter().any(|key| key == "execution_mode"));
    assert!(!keys.iter().any(|key| key == "task_id"));
}

#[test]
fn given_schema_when_required_fields_read_then_neither_prompt_nor_tasks_is_required() {
    assert!(TASK_TOOL_PARAMS.get("required").is_none());
}

#[test]
fn given_batch_task_parameters_when_inspected_then_finite_maximum_is_enforced() {
    assert_eq!(
        TASK_TOOL_PARAMS["properties"]["tasks"]["maxItems"],
        json!(MAX_TASK_BATCH_ITEMS)
    );
}

#[test]
fn given_schema_when_task_summary_inspected_then_it_follows_prompt_with_length_limit() {
    let keys = property_keys(&TASK_TOOL_PARAMS);
    assert_eq!(index_of(&keys, "task_summary"), index_of(&keys, "prompt") + 1);
    assert_eq!(
        TASK_TOOL_PARAMS["properties"]["task_summary"]["maxLength"],
        json!(TASK_SUMMARY_MAX_LENGTH)
    );
}

#[test]
fn given_batch_item_schema_when_task_summary_inspected_then_it_follows_prompt_with_length_limit() {
    let item_schema = &TASK_TOOL_PARAMS["properties"]["tasks"]["items"];
    let keys = property_keys(item_schema);
    assert_eq!(index_of(&keys, "task_summary"), index_of(&keys, "prompt") + 1);
    assert_eq!(
        item_schema["properties"]["task_summary"]["maxLength"],
        json!(TASK_SUMMARY_MAX_LENGTH)
    );
}
