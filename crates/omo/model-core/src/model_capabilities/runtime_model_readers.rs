use serde_json::Map;
use serde_json::Value;

use super::types::SnapshotModalities;

type Record = Map<String, Value>;

fn read_string_array(value: Option<&Value>) -> Option<Vec<String>> {
    let strings: Vec<String> = value?
        .as_array()?
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_string)
        .collect();
    if strings.is_empty() {
        None
    } else {
        Some(strings)
    }
}

fn lowercase_all(values: Vec<String>) -> Vec<String> {
    values
        .into_iter()
        .map(|value| value.to_lowercase())
        .collect()
}

fn normalize_variant_keys(value: Option<&Value>) -> Option<Vec<String>> {
    if let Some(variants) = read_string_array(value) {
        return Some(lowercase_all(variants));
    }
    let variants: Vec<String> = value?
        .as_object()?
        .keys()
        .map(|variant| variant.to_lowercase())
        .collect();
    if variants.is_empty() {
        None
    } else {
        Some(variants)
    }
}

fn read_modality_keys(value: Option<&Value>) -> Option<Vec<String>> {
    if let Some(modalities) = read_string_array(value) {
        return Some(lowercase_all(modalities));
    }
    let record = value?.as_object()?;

    // OpenCode's object-shaped modalities ({ input: string[], output: string[] }) reach here via
    // the capabilities fallback; flatten nested string arrays first.
    let from_nested: Vec<String> = record
        .values()
        .filter_map(Value::as_array)
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_lowercase)
        .collect();
    if !from_nested.is_empty() {
        return Some(from_nested);
    }

    let enabled: Vec<String> = record
        .iter()
        .filter(|(_, supported)| supported.as_bool() == Some(true))
        .map(|(modality, _)| modality.to_lowercase())
        .collect();
    if enabled.is_empty() {
        None
    } else {
        Some(enabled)
    }
}

fn normalize_modalities(value: Option<&Value>) -> Option<SnapshotModalities> {
    let record = value?.as_object()?;
    let input = read_modality_keys(record.get("input"));
    let output = read_modality_keys(record.get("output"));
    if input.is_none() && output.is_none() {
        return None;
    }
    Some(SnapshotModalities { input, output })
}

fn read_runtime_model_capabilities(runtime_model: Option<&Record>) -> Option<&Record> {
    runtime_model?.get("capabilities")?.as_object()
}

fn read_runtime_model_boolean(runtime_model: Option<&Record>, keys: &[&str]) -> Option<bool> {
    let runtime_capabilities = read_runtime_model_capabilities(runtime_model);
    keys.iter().find_map(|key| {
        runtime_model
            .and_then(|model| model.get(*key))
            .and_then(Value::as_bool)
            .or_else(|| {
                runtime_capabilities
                    .and_then(|capabilities| capabilities.get(*key))
                    .and_then(Value::as_bool)
            })
    })
}

pub(super) fn read_runtime_model_variants(runtime_model: Option<&Record>) -> Option<Vec<String>> {
    normalize_variant_keys(runtime_model.and_then(|model| model.get("variants"))).or_else(|| {
        normalize_variant_keys(read_runtime_model_capabilities(runtime_model)?.get("variants"))
    })
}

pub(super) fn read_runtime_model_modalities(
    runtime_model: Option<&Record>,
) -> Option<SnapshotModalities> {
    if let Some(root_modalities) =
        normalize_modalities(runtime_model.and_then(|model| model.get("modalities")))
    {
        return Some(root_modalities);
    }
    let runtime_capabilities = read_runtime_model_capabilities(runtime_model)?;
    normalize_modalities(runtime_capabilities.get("modalities"))
        .or_else(|| normalize_modalities(Some(&Value::Object(runtime_capabilities.clone()))))
}

pub(super) fn read_runtime_model_reasoning_support(runtime_model: Option<&Record>) -> Option<bool> {
    read_runtime_model_boolean(runtime_model, &["reasoning"])
}

pub(super) fn read_runtime_model_thinking_support(runtime_model: Option<&Record>) -> Option<bool> {
    read_runtime_model_reasoning_support(runtime_model)
        .or_else(|| read_runtime_model_boolean(runtime_model, &["thinking", "supportsThinking"]))
}

pub(super) fn read_runtime_model_temperature_support(
    runtime_model: Option<&Record>,
) -> Option<bool> {
    read_runtime_model_boolean(runtime_model, &["temperature"])
}

pub(super) fn read_runtime_model_top_p_support(runtime_model: Option<&Record>) -> Option<bool> {
    read_runtime_model_boolean(runtime_model, &["topP", "top_p"])
}

pub(super) fn read_runtime_model_tool_call_support(runtime_model: Option<&Record>) -> Option<bool> {
    read_runtime_model_boolean(runtime_model, &["toolCall", "tool_call", "toolcall"])
}

pub(super) fn read_runtime_model_limit_output(runtime_model: Option<&Record>) -> Option<i64> {
    let limit = match runtime_model
        .and_then(|model| model.get("limit"))
        .and_then(Value::as_object)
    {
        Some(limit) => limit,
        None => read_runtime_model_capabilities(runtime_model)?
            .get("limit")?
            .as_object()?,
    };
    // 0 or negative is unknown so callers fall back to the snapshot.
    limit.get("output")?.as_i64().filter(|output| *output > 0)
}
