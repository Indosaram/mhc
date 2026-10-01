//! Port of senpi packages/coding-agent/src/core/experimental.ts.

use crate::brand::env_value;
use crate::config::current_env;

/// PREFER_STRICT_TOOL_SAMPLING, returned only when experimental features are enabled.
pub fn get_experimental_tool_sampling() -> Option<serde_json::Value> {
    tool_sampling_for(are_experimental_features_enabled())
}

fn tool_sampling_for(enabled: bool) -> Option<serde_json::Value> {
    if !enabled {
        return None;
    }
    Some(serde_json::json!({ "type": "json_schema", "strict": "prefer" }))
}

pub fn are_experimental_features_enabled() -> bool {
    env_value("EXPERIMENTAL", &current_env()).as_deref() == Some("1")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sampling_hint_matches_senpi_when_enabled() {
        assert_eq!(
            tool_sampling_for(true),
            Some(serde_json::json!({ "type": "json_schema", "strict": "prefer" }))
        );
        assert_eq!(tool_sampling_for(false), None);
    }

    #[test]
    fn the_public_hint_follows_the_gate() {
        assert_eq!(get_experimental_tool_sampling(), tool_sampling_for(are_experimental_features_enabled()));
    }

    #[test]
    fn the_gate_reads_the_brand_experimental_var() {
        assert_eq!(
            are_experimental_features_enabled(),
            env_value("EXPERIMENTAL", &current_env()).as_deref() == Some("1")
        );
    }
}
