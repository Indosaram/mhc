use serde_json::Value;

use crate::internal::validate::{Field, Node, boolean, defaulted, strict_object};

pub const TELEMETRY_HARNESS_SUPPORT: [(&str, &[&str]); 1] = [("telemetry.enabled", &["native"])];

// SAFETY-DIVERGENCE(oh-my-openagent packages/config-core/src/schema/telemetry.ts): telemetry.enabled defaults to true; mhc defaults to false (opt-in).
fn default_false() -> Value {
    Value::Bool(false)
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
    strict_object(vec![defaulted("enabled", boolean(), default_false)])
}

pub fn is_omo_telemetry_enabled(config: &Value) -> bool {
    // SAFETY-DIVERGENCE(oh-my-openagent packages/config-core/src/schema/telemetry.ts): telemetry resolution defaults to true; mhc defaults to false (opt-in).
    config
        .get("telemetry")
        .and_then(|telemetry| telemetry.get("enabled"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}
