//! Structural validation of the omo codegraph config and harness override blocks.

use serde_json::Value;

use omo_config_core::codegraph_setting_harness_support;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OmoConfigValidationResult {
    pub errors: Vec<String>,
    pub ok: bool,
}

const SESSION_START_COOLDOWN_FLOOR_MS: f64 = 60_000.0;

#[derive(Clone, Copy)]
enum ValueType {
    Boolean,
    Number,
    String,
    StringArray,
}

impl ValueType {
    const fn label(self) -> &'static str {
        match self {
            ValueType::Boolean => "boolean",
            ValueType::Number => "number",
            ValueType::String => "string",
            ValueType::StringArray => "string_array",
        }
    }

    fn matches(self, value: &Value) -> bool {
        match self {
            ValueType::Boolean => value.is_boolean(),
            ValueType::Number => value.is_number(),
            ValueType::String => value.is_string(),
            ValueType::StringArray => value
                .as_array()
                .is_some_and(|items| items.iter().all(Value::is_string)),
        }
    }
}

fn codegraph_value_type(key: &str) -> Option<ValueType> {
    match key {
        "auto_provision" | "daemon" | "enabled" | "telemetry" => Some(ValueType::Boolean),
        "excluded_roots" => Some(ValueType::StringArray),
        "install_dir" => Some(ValueType::String),
        "session_start_cooldown_ms" | "watch_debounce_ms" => Some(ValueType::Number),
        _ => None,
    }
}

fn harness_for_block(key: &str) -> Option<&'static str> {
    match key {
        "[codex]" => Some("codex"),
        "[omo]" => Some("omo"),
        "[opencode]" => Some("opencode"),
        _ => None,
    }
}

fn validate_codegraph(
    section: &Value,
    prefix: &str,
    harness: Option<&str>,
    errors: &mut Vec<String>,
) {
    let Some(section) = section.as_object() else {
        errors.push(format!("{prefix} must be an object"));
        return;
    };
    for (key, value) in section {
        let Some(expected) = codegraph_value_type(key) else {
            errors.push(format!("{prefix}.{key} is not a supported setting"));
            continue;
        };
        if !expected.matches(value) {
            let message = match expected {
                ValueType::StringArray => format!("{prefix}.{key} must be an array of strings"),
                ValueType::Boolean | ValueType::Number | ValueType::String => {
                    format!("{prefix}.{key} must be a {}", expected.label())
                }
            };
            errors.push(message);
            continue;
        }
        let number = value.as_f64();
        if key == "watch_debounce_ms" && number.is_some_and(|n| !n.is_finite() || n < 0.0) {
            errors.push(format!(
                "{prefix}.{key} must be a non-negative finite number"
            ));
            continue;
        }
        if key == "session_start_cooldown_ms"
            && number.is_some_and(|n| !n.is_finite() || n < SESSION_START_COOLDOWN_FLOOR_MS)
        {
            errors.push(format!(
                "{prefix}.{key} must be a finite number of at least 60000"
            ));
            continue;
        }
        if let Some(harness) = harness {
            let setting_path = format!("codegraph.{key}");
            let supported = codegraph_setting_harness_support(&setting_path)
                .is_some_and(|ids| ids.contains(&harness));
            if !supported {
                errors.push(format!(
                    "{setting_path} is not supported for harness {harness}"
                ));
            }
        }
    }
}

fn validate_body(value: &Value, prefix: &str, harness: Option<&str>, errors: &mut Vec<String>) {
    let Some(body) = value.as_object() else {
        errors.push(format!("{prefix} must be an object"));
        return;
    };
    for (key, section) in body {
        if key == "codegraph" {
            validate_codegraph(section, &format!("{prefix}.codegraph"), harness, errors);
            continue;
        }
        if key.starts_with('[') && key.ends_with(']') {
            if harness.is_some() {
                errors.push(format!(
                    "{prefix}.{key} cannot contain nested harness override blocks"
                ));
                continue;
            }
            match harness_for_block(key) {
                Some(id) => validate_body(section, key, Some(id), errors),
                None => errors.push(format!("Unknown harness override block \"{key}\"")),
            }
            continue;
        }
        errors.push(format!("{prefix}.{key} is not a supported setting"));
    }
}

pub fn validate_omo_config(value: &Value) -> OmoConfigValidationResult {
    let mut errors = Vec::new();
    validate_body(value, "config", None, &mut errors);
    OmoConfigValidationResult {
        ok: errors.is_empty(),
        errors,
    }
}
