//! Port of senpi `packages/agent/src/execution/canonical-json.ts`.

use serde_json::Value;

/// JSON with object keys sorted, so two values with the same fields compare equal whatever their key order.
pub fn canonical_json(value: &Value) -> String {
    match value {
        Value::Array(items) => {
            let parts: Vec<String> = items.iter().map(canonical_json).collect();
            format!("[{}]", parts.join(","))
        }
        Value::Object(map) => {
            let mut entries: Vec<(&String, &Value)> = map.iter().collect();
            entries.sort_by_key(|(a, _)| *a);
            let parts: Vec<String> = entries
                .into_iter()
                .map(|(key, inner)| {
                    format!(
                        "{}:{}",
                        serde_json::to_string(key).unwrap_or_else(|_| "\"\"".to_owned()),
                        canonical_json(inner)
                    )
                })
                .collect();
            format!("{{{}}}", parts.join(","))
        }
        _ => serde_json::to_string(value).unwrap_or_else(|_| "null".to_owned()),
    }
}
