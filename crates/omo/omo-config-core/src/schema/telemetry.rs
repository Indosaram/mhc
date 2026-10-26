use serde_json::Value;

use crate::internal::validate::{Field, Node, boolean, defaulted, number_between, strict_object};

pub const TELEMETRY_HARNESS_SUPPORT: [(&str, &[&str]); 2] = [
    ("telemetry.enabled", &["native"]),
    ("telemetry.rawTurnSampleRate", &["native"]),
];

// SAFETY-DIVERGENCE(oh-my-openagent packages/config-core/src/schema/telemetry.ts): telemetry.enabled defaults to true; mhc defaults to false (opt-in).
fn default_false() -> Value {
    Value::Bool(false)
}

pub fn omo_telemetry_settings_layer_schema() -> Node {
    strict_object(vec![
        Field {
            key: "enabled",
            node: boolean(),
            default: None,
            required: false,
        },
        Field {
            key: "rawTurnSampleRate",
            node: number_between(0.0, 1.0),
            default: None,
            required: false,
        },
    ])
}

pub fn omo_telemetry_settings_schema() -> Node {
    strict_object(vec![
        defaulted("enabled", boolean(), default_false),
        Field {
            key: "rawTurnSampleRate",
            node: number_between(0.0, 1.0),
            default: None,
            required: false,
        },
    ])
}

/// Share of sessions that also send every raw `turn_completed`; absent leaves the documented default.
pub fn omo_telemetry_raw_turn_sample_rate(config: &Value) -> Option<f64> {
    config
        .get("telemetry")
        .and_then(|telemetry| telemetry.get("rawTurnSampleRate"))
        .and_then(Value::as_f64)
        .filter(|rate| rate.is_finite() && (0.0..=1.0).contains(rate))
}

pub fn is_omo_telemetry_enabled(config: &Value) -> bool {
    // SAFETY-DIVERGENCE(oh-my-openagent packages/config-core/src/schema/telemetry.ts): telemetry resolution defaults to true; mhc defaults to false (opt-in).
    config
        .get("telemetry")
        .and_then(|telemetry| telemetry.get("enabled"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}
