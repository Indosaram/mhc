//! Structural validation of the omo harness override blocks.

use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OmoConfigValidationResult {
    pub errors: Vec<String>,
    pub ok: bool,
}

fn harness_for_block(key: &str) -> Option<&'static str> {
    match key {
        "[codex]" => Some("codex"),
        "[omo]" => Some("omo"),
        "[opencode]" => Some("opencode"),
        _ => None,
    }
}

fn validate_body(value: &Value, prefix: &str, harness: Option<&str>, errors: &mut Vec<String>) {
    let Some(body) = value.as_object() else {
        errors.push(format!("{prefix} must be an object"));
        return;
    };
    for (key, section) in body {
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
