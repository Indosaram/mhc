//! Port of senpi packages/ai/src/utils/tool-argument-validator.ts: a protocol may own tool
//! argument validation without permitting local argument coercion.

use serde_json::{Map, Value};
use std::collections::HashMap;
use std::sync::{Arc, LazyLock, RwLock};

/// `ToolArgumentValidationError` (Ajv-shaped).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ToolArgumentValidationError {
    pub instance_path: String,
    pub keyword: String,
    pub message: Option<String>,
    /// `params`, including the optional `missingProperty` / `requiredProperties`.
    pub params: Map<String, Value>,
}

impl ToolArgumentValidationError {
    pub fn missing_property(&self) -> Option<&str> {
        self.params.get("missingProperty").and_then(Value::as_str)
    }

    pub fn required_properties(&self) -> Option<Vec<&str>> {
        self.params
            .get("requiredProperties")
            .and_then(Value::as_array)
            .map(|items| items.iter().filter_map(Value::as_str).collect())
    }
}

/// `ToolArgumentValidator`: `Ok(())` when valid, else its errors (`validate.errors`).
pub type ToolArgumentValidator =
    Arc<dyn Fn(&Value) -> Result<(), Vec<ToolArgumentValidationError>> + Send + Sync>;

// The TS registry is a WeakMap keyed by schema object identity; Rust tool schemas are plain JSON
// values, so the registry is keyed by the schema's canonical JSON text.
static VALIDATORS: LazyLock<RwLock<HashMap<String, ToolArgumentValidator>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

fn schema_key(schema: &Value) -> String {
    schema.to_string()
}

pub fn register_tool_argument_validator(schema: &Value, validate: ToolArgumentValidator) {
    VALIDATORS
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(schema_key(schema), validate);
}

pub fn get_tool_argument_validator(schema: &Value) -> Option<ToolArgumentValidator> {
    VALIDATORS
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(&schema_key(schema))
        .cloned()
}
