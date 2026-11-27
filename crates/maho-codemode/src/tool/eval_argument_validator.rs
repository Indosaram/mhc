use super::{
    eval_language_inference::{EVAL_LANGUAGE_INFERRED_KEY, infer_eval_language},
    eval_request::{EvalRunField, eval_run_field_required_message, normalize_eval_summary},
    types::EvalLanguage,
};
use maho_ai::utils::{
    tool_argument_validator::{ToolArgumentValidationError, ToolArgumentValidator},
    validation::{register_tool_argument_validator, strict_issues},
};
use serde_json::{Map, Value};
use std::sync::Arc;

const RUN_FIELDS: [EvalRunField; 3] = [
    EvalRunField::Language,
    EvalRunField::Code,
    EvalRunField::Summary,
];

fn language_wire_name(language: EvalLanguage) -> &'static str {
    match language {
        EvalLanguage::Js => "js",
    }
}

fn is_missing(field: EvalRunField, value: Option<&Value>) -> bool {
    let Some(value) = value else {
        return true;
    };
    if value.is_null() {
        return true;
    }
    if field == EvalRunField::Summary {
        return value.is_string() && normalize_eval_summary(value).is_none();
    }
    false
}

fn fill_inferred_language(value: &mut Value, languages: &[EvalLanguage]) -> Option<&'static str> {
    let object = value.as_object_mut()?;
    let action = object.get("action").and_then(Value::as_str);
    if object.get("action").is_some() && action != Some("run") {
        return None;
    }
    object.remove(EVAL_LANGUAGE_INFERRED_KEY);
    if !is_missing(EvalRunField::Language, object.get("language")) {
        return None;
    }
    let inference = infer_eval_language(object.get("code").and_then(Value::as_str), languages)?;
    object.insert(
        "language".into(),
        Value::String(language_wire_name(inference.language).into()),
    );
    Some(match inference.basis {
        super::eval_language_inference::EvalLanguageInferenceBasis::OnlyEnabledLanguage => {
            "only-enabled-language"
        }
        super::eval_language_inference::EvalLanguageInferenceBasis::CodeMarkers => "code-markers",
    })
}

fn missing_run_field_errors(
    value: &Value,
    languages: &[EvalLanguage],
) -> Vec<ToolArgumentValidationError> {
    let Some(object) = value.as_object() else {
        return Vec::new();
    };
    if object.get("action").is_some() && object.get("action").and_then(Value::as_str) != Some("run")
    {
        return Vec::new();
    }
    RUN_FIELDS
        .iter()
        .copied()
        .filter(|field| is_missing(*field, object.get(field.name())))
        .map(|field| {
            let mut params = Map::new();
            params.insert("missingProperty".into(), Value::String(field.name().into()));
            ToolArgumentValidationError {
                instance_path: String::new(),
                keyword: "required".into(),
                message: Some(eval_run_field_required_message(field, languages)),
                params,
            }
        })
        .collect()
}

/// Owns eval argument validation (`senpi#5138`, `senpi#5154`). A run call (`action` omitted or
/// `"run"`) missing a run field fails with one teaching error per missing field before generic
/// `anyOf` validation dumps per-branch noise.
pub fn register_eval_argument_validator(schema: &Value, languages: &[EvalLanguage]) {
    let schema_copy = schema.clone();
    let languages = languages.to_vec();
    let validate: ToolArgumentValidator = Arc::new(move |value: &Value| {
        let mut candidate = value.clone();
        let inferred = fill_inferred_language(&mut candidate, &languages);
        let missing = missing_run_field_errors(&candidate, &languages);
        if !missing.is_empty() {
            return Err(missing);
        }
        // Check against the original schema without calling `validate_tool_arguments` recursively.
        let issues = strict_issues(&schema_copy, &candidate);
        if !issues.is_empty() {
            if inferred.is_some() {
                let raw_issues = strict_issues(&schema_copy, value);
                if !raw_issues.is_empty() {
                    return Err(raw_issues);
                }
            }
            return Err(issues);
        }
        Ok(())
    });
    register_tool_argument_validator(schema, validate);
}
