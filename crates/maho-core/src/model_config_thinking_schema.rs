//! Port of senpi `packages/coding-agent/src/core/model-config-thinking-schema.ts`.
//!
//! models.json thinking-level vocabulary: `thinkingLevelMap` and `defaultThinkingLevel` (senpi#2196).
//! The TypeBox schemas become validators over raw JSON values.

use serde_json::Value;

pub const THINKING_LEVELS: [&str; 7] = ["off", "minimal", "low", "medium", "high", "xhigh", "max"];

/// `ThinkingLevelSchema`: one of the seven level literals.
pub fn is_thinking_level(value: &Value) -> bool {
    value
        .as_str()
        .is_some_and(|level| THINKING_LEVELS.contains(&level))
}

/// `ThinkingLevelMapSchema`: an object whose known level keys hold a string or null. Like a TypeBox
/// `Type.Object`, keys outside the vocabulary are allowed.
pub fn is_thinking_level_map(value: &Value) -> bool {
    let Some(map) = value.as_object() else {
        return false;
    };
    THINKING_LEVELS.iter().all(|level| match map.get(*level) {
        None | Some(Value::Null) | Some(Value::String(_)) => true,
        Some(_) => false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn levels_are_the_seven_literals() {
        for level in THINKING_LEVELS {
            assert!(is_thinking_level(&json!(level)));
        }
        assert!(!is_thinking_level(&json!("extreme")));
        assert!(!is_thinking_level(&json!(1)));
    }

    #[test]
    fn map_values_are_strings_or_null() {
        assert!(is_thinking_level_map(&json!({})));
        assert!(is_thinking_level_map(
            &json!({"off": null, "high": "max-effort"})
        ));
        assert!(!is_thinking_level_map(&json!({"low": 1})));
        assert!(!is_thinking_level_map(&json!([])));
    }
}
