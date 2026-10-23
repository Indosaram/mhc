//! Port of senpi packages/coding-agent/src/core/engine-paused.ts.

use serde_json::Value;

use crate::engine_turn_limit::{ENGINE_TURN_START_ENTRY_TYPE, is_user_directed_turn};

pub const ENGINE_PAUSED_ENTRY_TYPE: &str = "engine-paused";

fn entry_type(entry: &Value) -> Option<&str> {
    entry.get("type").and_then(Value::as_str)
}

fn custom_type(entry: &Value) -> Option<&str> {
    entry.get("customType").and_then(Value::as_str)
}

fn is_user_message(entry: &Value) -> bool {
    entry_type(entry) == Some("message")
        && entry.pointer("/message/role").and_then(Value::as_str) == Some("user")
}

fn is_user_directed_custom_message(entry: &Value) -> bool {
    entry_type(entry) == Some("custom_message")
        && custom_type(entry).is_some_and(is_user_directed_turn)
}

/// A pause was recorded after the latest turn started and nothing since has lifted it.
pub fn engine_pause_since_last_turn(entries: &[Value]) -> bool {
    for entry in entries.iter().rev() {
        if is_user_message(entry) || is_user_directed_custom_message(entry) {
            return false;
        }
        if entry_type(entry) == Some("custom") {
            match custom_type(entry) {
                Some(ENGINE_TURN_START_ENTRY_TYPE) => return false,
                Some(ENGINE_PAUSED_ENTRY_TYPE) => return true,
                _ => {}
            }
        }
    }
    false
}

/// The latest engine turn was the repetitive-turns correction and has not already reported a stop.
pub fn settled_repetitive_turn_pause(entries: &[Value]) -> bool {
    let mut corrected = false;
    for entry in entries.iter().rev() {
        if is_user_message(entry) || is_user_directed_custom_message(entry) {
            return false;
        }
        if entry_type(entry) == Some("custom")
            && custom_type(entry) == Some(ENGINE_PAUSED_ENTRY_TYPE)
        {
            return false;
        }
        if entry_type(entry) == Some("custom")
            && custom_type(entry) == Some(ENGINE_TURN_START_ENTRY_TYPE)
        {
            return corrected
                && entry.pointer("/data/customType").and_then(Value::as_str)
                    == Some("ttsr-injection");
        }
        if entry_type(entry) != Some("custom_message")
            || custom_type(entry) != Some("ttsr-injection")
        {
            continue;
        }
        corrected = entry
            .pointer("/details/rules")
            .and_then(Value::as_array)
            .is_some_and(|rules| {
                rules
                    .iter()
                    .any(|rule| rule.as_str() == Some("repetitive-turns"))
            });
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn custom(custom_type: &str, data: Value) -> Value {
        json!({"type": "custom", "customType": custom_type, "data": data})
    }

    #[test]
    fn a_pause_holds_until_a_turn_starts_or_the_user_speaks() {
        let paused = vec![custom(ENGINE_PAUSED_ENTRY_TYPE, json!({}))];
        assert!(engine_pause_since_last_turn(&paused));
        let mut started = paused.clone();
        started.push(custom(ENGINE_TURN_START_ENTRY_TYPE, json!({})));
        assert!(!engine_pause_since_last_turn(&started));
        let mut spoke = paused;
        spoke.push(json!({"type": "message", "message": {"role": "user"}}));
        assert!(!engine_pause_since_last_turn(&spoke));
    }

    #[test]
    fn a_repetition_correction_settles_into_one_pause() {
        let entries = vec![
            custom(
                ENGINE_TURN_START_ENTRY_TYPE,
                json!({"customType": "ttsr-injection"}),
            ),
            json!({"type": "custom_message", "customType": "ttsr-injection", "details": {"rules": ["repetitive-turns"]}}),
        ];
        assert!(settled_repetitive_turn_pause(&entries));
        let mut paused = entries;
        paused.push(custom(ENGINE_PAUSED_ENTRY_TYPE, json!({})));
        assert!(!settled_repetitive_turn_pause(&paused));
    }
}
