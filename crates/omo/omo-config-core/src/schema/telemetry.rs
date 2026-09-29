use serde_json::Value;

use crate::internal::validate::{Field, Node, boolean, defaulted, strict_object};

pub const TELEMETRY_HARNESS_SUPPORT: [(&str, &[&str]); 1] = [("telemetry.enabled", &["senpi"])];

fn default_true() -> Value {
    Value::Bool(true)
}

pub fn omo_telemetry_settings_layer_schema() -> Node {
    strict_object(vec![Field {
        key: "enabled",
        node: boolean(),
        default: None,
        required: false,
    }])
}

pub fn omo_telemetry_settings_schema() -> Node {
    strict_object(vec![defaulted("enabled", boolean(), default_true)])
}

pub fn is_omo_telemetry_enabled(config: &Value) -> bool {
    config
        .get("telemetry")
        .and_then(|telemetry| telemetry.get("enabled"))
        .and_then(Value::as_bool)
        .unwrap_or(true)
}
