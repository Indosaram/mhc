//! Per-identity `<external_projection>` limits (port of `projection-limits.ts`).

use memory_core::compile::external_projection::ExternalProjectionLimits;

/// `{ ...settings.projection, ...settings.agents[identity]?.projection }`: a per-agent layer
/// overrides only the keys it sets and keeps the other base key.
pub fn projection_limits(settings: &serde_json::Value, identity: &str) -> ExternalProjectionLimits {
    let base = &settings["projection"];
    let layer = &settings["agents"][identity]["projection"];
    let key = |name: &str| {
        layer[name]
            .as_u64()
            .or_else(|| base[name].as_u64())
            .unwrap_or_default() as usize
    };
    ExternalProjectionLimits {
        max_entries_per_directory: key("max_entries_per_directory"),
        max_bytes: key("max_bytes"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_per_agent_override_of_one_key_keeps_the_other_base_key() {
        // Given
        let settings = serde_json::json!({
            "projection": { "max_entries_per_directory": 40, "max_bytes": 8000 },
            "agents": { "scout": { "projection": { "max_entries_per_directory": 5 } } },
        });
        // When
        let scout = projection_limits(&settings, "scout");
        let other = projection_limits(&settings, "builder");
        // Then
        assert_eq!(
            scout,
            ExternalProjectionLimits {
                max_entries_per_directory: 5,
                max_bytes: 8000
            }
        );
        assert_eq!(
            other,
            ExternalProjectionLimits {
                max_entries_per_directory: 40,
                max_bytes: 8000
            }
        );
    }
}
