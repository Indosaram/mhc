use serde_json::Value;

pub const MAX_ENRICHED_TOOL_CALLS: usize = 30;
pub const MAX_AGGREGATED_TOOL_NAMES: usize = 64;
pub const MAX_CAPTURED_IDENTIFIER_CODE_POINTS: usize = 128;
pub const MAX_CAPTURED_TOOL_NAME_CODE_POINTS: usize = 128;
pub const MAX_RPC_EVENT_BYTES: usize = 32 * 1024;

pub fn cap_code_points(text: &str, max: usize) -> String {
    if text.chars().count() <= max { text.into() } else { format!("{}…", text.chars().take(max).collect::<String>()) }
}

#[derive(Debug, PartialEq)]
pub struct BoundedToolCallArgs { pub args: Option<Value>, pub truncated: bool }

pub fn bound_tool_call_args(args: &Value) -> BoundedToolCallArgs {
    let (value, truncated) = bound_value(args, 0);
    if value.to_string().encode_utf16().count() > 4096 {
        BoundedToolCallArgs { args: None, truncated: true }
    } else { BoundedToolCallArgs { args: Some(value), truncated } }
}

fn bound_value(value: &Value, depth: usize) -> (Value, bool) {
    match value {
        Value::String(text) => {
            let capped = cap_code_points(text, 512);
            let truncated = capped != *text;
            (Value::String(capped), truncated)
        }
        Value::Array(_) | Value::Object(_) if depth >= 6 => (Value::String("…".into()), true),
        Value::Array(values) => {
            let mut truncated = values.len() > 32;
            let values = values.iter().take(32).map(|value| {
                let (value, nested_truncated) = bound_value(value, depth + 1);
                truncated |= nested_truncated;
                value
            }).collect();
            (Value::Array(values), truncated)
        }
        Value::Object(values) => {
            let mut truncated = values.len() > 32;
            let values = values.iter().take(32).map(|(key, value)| {
                let (value, nested_truncated) = bound_value(value, depth + 1);
                truncated |= nested_truncated;
                (key.clone(), value)
            }).collect();
            (Value::Object(values), truncated)
        }
        _ => (value.clone(), false),
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct EvalToolCallMetric {
    pub name: String,
    pub started_at: f64,
    pub ok: Option<bool>,
    pub duration_ms: Option<f64>,
}

pub fn create_tool_call_metric(name: &str, started_at: f64) -> EvalToolCallMetric {
    EvalToolCallMetric { name: cap_code_points(name, MAX_CAPTURED_TOOL_NAME_CODE_POINTS), started_at, ok: None, duration_ms: None }
}

pub fn settle_tool_call_metric(metric: &mut EvalToolCallMetric, ok: bool, completed_at: f64) {
    metric.ok = Some(ok);
    metric.duration_ms = Some((completed_at - metric.started_at).max(0.0));
}
