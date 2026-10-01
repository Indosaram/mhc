//! `tools/task/argument-normalization.ts`: tolerant normalization of raw task tool arguments.
//! Produces the normalized params as a JSON object (same keys and order as the TS spread).

use serde_json::{Map, Value};

use crate::task_summary::clamp_task_summary;

const PROVIDER_PADDING_PROMPTS: [&str; 4] = ["unused", "placeholder", "not used", "n/a"];

fn non_blank_text(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())
        .map(str::to_string)
}

fn identifier(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

fn string_list(value: Option<&Value>) -> Option<Vec<String>> {
    let entries = value?.as_array()?;
    let strings: Vec<String> = entries
        .iter()
        .filter_map(|entry| identifier(Some(entry)))
        .collect();
    (!strings.is_empty()).then_some(strings)
}

fn summary_text(value: Option<&Value>) -> Option<String> {
    clamp_task_summary(value.and_then(Value::as_str))
}

fn put_text(map: &mut Map<String, Value>, key: &str, value: Option<String>) {
    if let Some(value) = value {
        map.insert(key.to_string(), Value::String(value));
    }
}

fn put_list(map: &mut Map<String, Value>, key: &str, value: Option<Vec<String>>) {
    if let Some(values) = value {
        map.insert(
            key.to_string(),
            Value::Array(values.into_iter().map(Value::String).collect()),
        );
    }
}

fn task_item(value: &Value) -> Option<Map<String, Value>> {
    let record = value.as_object()?;
    let prompt = non_blank_text(record.get("prompt"))?;
    let mut item = Map::new();
    item.insert("prompt".to_string(), Value::String(prompt));
    put_text(&mut item, "task_summary", summary_text(record.get("task_summary")));
    put_text(&mut item, "description", non_blank_text(record.get("description")));
    put_text(&mut item, "category", identifier(record.get("category")));
    put_text(&mut item, "subagent_type", identifier(record.get("subagent_type")));
    put_text(&mut item, "name", identifier(record.get("name")));
    put_text(&mut item, "model", identifier(record.get("model")));
    put_list(&mut item, "load_skills", string_list(record.get("load_skills")));
    Some(item)
}

fn task_items(value: Option<&Value>) -> Option<Vec<Map<String, Value>>> {
    Some(value?.as_array()?.iter().filter_map(task_item).collect())
}

fn is_provider_padding_task(item: &Map<String, Value>) -> bool {
    let prompt = item
        .get("prompt")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_lowercase();
    PROVIDER_PADDING_PROMPTS.contains(&prompt.as_str())
}

pub fn normalize_task_tool_arguments(raw: &Value) -> Value {
    let Some(raw) = raw.as_object() else {
        return Value::Object(Map::new());
    };

    let prompt = non_blank_text(raw.get("prompt"));
    let normalized_tasks = task_items(raw.get("tasks"));
    let tasks_are_single_padding = prompt.is_some()
        && normalized_tasks
            .as_ref()
            .is_some_and(|tasks| tasks.is_empty() || tasks.iter().all(is_provider_padding_task));
    let tasks = if tasks_are_single_padding {
        None
    } else {
        normalized_tasks
    };
    let run_in_background = raw.get("run_in_background").and_then(Value::as_bool);

    let mut out = Map::new();
    put_text(&mut out, "prompt", prompt);
    put_text(&mut out, "task_summary", summary_text(raw.get("task_summary")));
    put_text(&mut out, "description", non_blank_text(raw.get("description")));
    put_text(&mut out, "category", identifier(raw.get("category")));
    put_text(&mut out, "subagent_type", identifier(raw.get("subagent_type")));
    if let Some(run_in_background) = run_in_background {
        out.insert("run_in_background".to_string(), Value::Bool(run_in_background));
    }
    put_text(&mut out, "name", identifier(raw.get("name")));
    put_text(&mut out, "model", identifier(raw.get("model")));
    put_list(&mut out, "load_skills", string_list(raw.get("load_skills")));
    if let Some(tasks) = tasks {
        out.insert(
            "tasks".to_string(),
            Value::Array(tasks.into_iter().map(Value::Object).collect()),
        );
    }
    Value::Object(out)
}
