//! Port of senpi packages/ai/src/api/anthropic-request-shape-log.ts.
//!
//! Debug-only request skeleton: roles, block types and tool ids per index, never content.

use serde_json::{Map, Value, json};

use crate::types::ProviderEnv;
use crate::utils::provider_env::get_provider_env_value;

pub const REQUEST_SHAPE_LOG_ENV: &str = "MAHO_DEBUG_REQUEST_SHAPE";

fn skeleton_block(block: &Value) -> Value {
    let Some(record) = block.as_object() else {
        let kind = match block {
            Value::Null => "object",
            Value::Bool(_) => "boolean",
            Value::Number(_) => "number",
            Value::String(_) => "string",
            _ => "object",
        };
        return json!({ "type": kind });
    };
    let mut out = Map::new();
    out.insert(
        "type".into(),
        json!(
            record
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
        ),
    );
    if let Some(id) = record.get("id").and_then(Value::as_str) {
        out.insert("id".into(), json!(id));
    }
    if let Some(id) = record.get("tool_use_id").and_then(Value::as_str) {
        out.insert("toolUseId".into(), json!(id));
    }
    Value::Object(out)
}

/// `anthropicRequestSkeleton`.
pub fn anthropic_request_skeleton(params: &Map<String, Value>) -> Vec<Value> {
    let messages = params
        .get("messages")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    messages
        .iter()
        .enumerate()
        .map(|(index, message)| {
            let Some(message) = message.as_object() else {
                return json!({ "index": index, "role": "unknown", "blocks": [] });
            };
            let role = message
                .get("role")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let blocks = match message.get("content").and_then(Value::as_array) {
                Some(content) => Value::Array(content.iter().map(skeleton_block).collect()),
                None => json!("string"),
            };
            json!({ "index": index, "role": role, "blocks": blocks })
        })
        .collect()
}

fn append_file(path: &str, data: &str) {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    // Debug logging must never fail a request.
    if let Ok(mut file) = options.open(path) {
        let _ = file.write_all(data.as_bytes());
    }
}

type AppendLine<'a> = &'a dyn Fn(&str, &str);

/// `logAnthropicRequestShape`: a no-op unless the env var names a file; appends one JSON line.
pub fn log_anthropic_request_shape(
    provider: &str,
    model_id: &str,
    params: &Map<String, Value>,
    env: Option<&ProviderEnv>,
    append: Option<AppendLine<'_>>,
) {
    let Some(path) = get_provider_env_value(REQUEST_SHAPE_LOG_ENV, env) else {
        return;
    };
    let ts = chrono::DateTime::from_timestamp_millis(crate::utils::diagnostics::now_ms())
        .map(|time| time.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
        .unwrap_or_default();
    let record = json!({
        "ts": ts,
        "provider": provider,
        "model": model_id,
        "messages": anthropic_request_skeleton(params),
    });
    let line = format!("{record}\n");
    match append {
        Some(append) => append(&path, &line),
        None => append_file(&path, &line),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn params() -> Map<String, Value> {
        json!({"messages": [
            {"role": "user", "content": "secret text"},
            {"role": "assistant", "content": [{"type": "tool_use", "id": "t1", "input": {"x": "secret"}}]},
            {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "t1", "content": "secret"}]},
            "bogus"
        ]})
        .as_object()
        .cloned()
        .unwrap_or_default()
    }

    #[test]
    fn skeleton_has_ids_and_never_content() {
        let skeleton = anthropic_request_skeleton(&params());
        assert_eq!(
            skeleton[0],
            json!({"index": 0, "role": "user", "blocks": "string"})
        );
        assert_eq!(
            skeleton[1]["blocks"],
            json!([{"type": "tool_use", "id": "t1"}])
        );
        assert_eq!(
            skeleton[2]["blocks"],
            json!([{"type": "tool_result", "toolUseId": "t1"}])
        );
        assert_eq!(
            skeleton[3],
            json!({"index": 3, "role": "unknown", "blocks": []})
        );
        assert!(!Value::Array(skeleton).to_string().contains("secret"));
    }

    #[test]
    fn logs_one_line_only_when_env_is_set() {
        let written: RefCell<Vec<(String, String)>> = RefCell::new(Vec::new());
        let append = |path: &str, data: &str| {
            written
                .borrow_mut()
                .push((path.to_owned(), data.to_owned()))
        };
        let mut env = ProviderEnv::new();
        env.insert(REQUEST_SHAPE_LOG_ENV.into(), String::new());
        log_anthropic_request_shape("anthropic", "m", &params(), Some(&env), Some(&append));
        assert!(written.borrow().is_empty());
        env.insert(REQUEST_SHAPE_LOG_ENV.into(), "/tmp/shape.log".into());
        log_anthropic_request_shape("anthropic", "m", &params(), Some(&env), Some(&append));
        let written = written.borrow();
        assert_eq!(written.len(), 1);
        assert_eq!(written[0].0, "/tmp/shape.log");
        assert!(written[0].1.ends_with('\n'));
        let record: Value = serde_json::from_str(written[0].1.trim()).unwrap_or_default();
        assert_eq!(record["provider"], json!("anthropic"));
        assert_eq!(record["model"], json!("m"));
    }
}
