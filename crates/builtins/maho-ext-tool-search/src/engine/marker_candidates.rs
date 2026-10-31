//! senpi `tool-search/engine/marker-candidates.ts`: find the history messages that can hold an
//! activation marker without serializing each one.
use serde_json::Value;

const ACTIVATION_MARKER_PREFIX: &str = "[tool_search:activated";

/// Whether `message.to_string()` could contain an activation marker, without serializing it.
///
/// Exact for JSON data: the marker has no character JSON escapes and cannot be formed by JSON
/// punctuation (after `[` JSON allows only a value, never `t` followed by `ool_`), so it appears
/// in the serialized blob only inside one key or string value. `serde_json::Value` has no
/// `toJSON` hook and cannot be cyclic, so upstream's `toJSON` candidate and `seen` set have no
/// counterpart here.
pub fn may_hold_activation_marker(message: &Value) -> bool {
    match message {
        Value::String(text) => text.contains(ACTIVATION_MARKER_PREFIX),
        Value::Array(items) => items.iter().any(may_hold_activation_marker),
        Value::Object(map) => map.iter().any(|(key, item)| {
            key.contains(ACTIVATION_MARKER_PREFIX) || may_hold_activation_marker(item)
        }),
        Value::Null | Value::Bool(_) | Value::Number(_) => false,
    }
}

/// senpi `activationMarkerCandidates`, without the event-loop slicer: the Rust history scan runs
/// on a blocking caller, so there is no host loop to yield to between messages.
pub fn activation_marker_candidates(messages: &[Value]) -> Vec<&Value> {
    messages
        .iter()
        .filter(|message| may_hold_activation_marker(message))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::marker::{
        LEGACY_TOOL_SEARCH_ACTIVATION_MARKER, ToolActivationIdentity, emit_activation_marker,
    };
    use serde_json::json;

    fn v2_marker() -> String {
        emit_activation_marker(&[ToolActivationIdentity {
            name: "weather_forecast".into(),
            registration_id: "/workspace/extensions/weather-tools.ts\0weather_forecast".into(),
        }])
        .expect("marker")
    }

    #[test]
    fn flags_every_message_whose_serialization_holds_a_marker_and_only_those() {
        let v2 = v2_marker();
        let with_marker = [
            json!({"role": "toolResult", "content": [{"type": "text", "text": format!("found {v2}")}]}),
            json!({"content": format!("{LEGACY_TOOL_SEARCH_ACTIVATION_MARKER} mcp_a mcp_b")}),
            json!({"deep": {"list": [1, null, {"text": format!("quoted \"{v2}\"")}]}}),
            json!({LEGACY_TOOL_SEARCH_ACTIVATION_MARKER: true}),
        ];
        let without_marker = [
            json!({"role": "user", "content": [{"type": "text", "text": "[tool_search] activated"}]}),
            json!({"list": ["[tool", "_search:activated"]}),
            json!({"missing": null}),
        ];
        for message in &with_marker {
            assert!(may_hold_activation_marker(message), "{message}");
        }
        for message in &without_marker {
            assert!(!may_hold_activation_marker(message), "{message}");
        }
        for message in with_marker.iter().chain(&without_marker) {
            assert_eq!(
                message.to_string().contains(ACTIVATION_MARKER_PREFIX),
                may_hold_activation_marker(message)
            );
        }
    }

    #[test]
    fn candidates_keep_only_marker_messages_in_order() {
        let v2 = v2_marker();
        let mut history: Vec<Value> = (0..500)
            .map(|index| json!({"role": "user", "content": format!("message {index}")}))
            .collect();
        history.push(json!({"role": "toolResult", "content": [{"type": "text", "text": v2}]}));
        let candidates = activation_marker_candidates(&history);
        assert_eq!(candidates, vec![&history[500]]);
    }
}
