//! Per-identity runtime retention and cap resolution from `memory` settings (`memory-runtime-retention.ts`).

use memory_core::retention::{
    DEFAULT_RUNTIME_MAX_BYTES, DEFAULT_RUNTIME_RETENTION, RuntimeRetentionPolicy,
    resolve_foreign_identity_max_bytes, resolve_foreign_identity_retention_policy,
};
use serde_json::Value;

pub use memory_core::retention::resolve_foreign_identity_discard_unreflected as resolve_discard_unreflected;

/// `memory.runtime` for one identity this session binds: `agents.<id>.runtime` wins field by field.
pub fn resolve_runtime_retention_policy(
    settings: &Value,
    identity: &str,
) -> RuntimeRetentionPolicy {
    if settings.is_null() {
        return DEFAULT_RUNTIME_RETENTION;
    }
    resolve_foreign_identity_retention_policy(Some(settings), identity)
}

/// `memory.runtime.max_bytes` for one identity (`0` = unbounded): `agents.<id>.runtime` wins.
pub fn resolve_runtime_max_bytes(settings: &Value, identity: &str) -> u64 {
    if settings.is_null() {
        return DEFAULT_RUNTIME_MAX_BYTES;
    }
    resolve_foreign_identity_max_bytes(Some(settings), identity)
}

/// `memory.reflection.enabled` for one identity (`agents.<id>.reflection.enabled` wins).
pub fn resolve_agent_reflection_enabled(settings: &Value, identity: &str) -> bool {
    let agent_enabled = settings
        .get("agents")
        .and_then(|agents| agents.get(identity))
        .and_then(|agent| agent.get("reflection"))
        .and_then(|reflection| reflection.get("enabled"))
        .and_then(Value::as_bool);
    if let Some(enabled) = agent_enabled {
        return enabled;
    }
    settings
        .get("reflection")
        .and_then(|reflection| reflection.get("enabled"))
        .and_then(Value::as_bool)
        .unwrap_or(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn agent_override_wins_field_by_field_over_global_runtime_settings() {
        let settings = json!({
            "runtime": {
                "retention_days": 14,
                "abandoned_journal_days": 10,
                "discard_unreflected_journals": true,
                "completions_days": 30,
                "pass_budget_ms": 5000,
                "max_bytes": 4096
            },
            "agents": {
                "oracle": {
                    "runtime": {
                        "retention_days": 21,
                        "discard_unreflected_journals": false,
                        "max_bytes": 0
                    },
                    "reflection": {
                        "enabled": false
                    }
                }
            }
        });
        let policy = resolve_runtime_retention_policy(&settings, "oracle");
        assert_eq!(policy.retention_days, 21);
        assert_eq!(policy.abandoned_journal_days, 10);
        assert!(!policy.discard_unreflected_journals);
        assert_eq!(policy.completions_days, 30);
        assert_eq!(policy.pass_budget_ms, 5000);
        assert_eq!(resolve_runtime_max_bytes(&settings, "oracle"), 0);
        assert!(!resolve_agent_reflection_enabled(&settings, "oracle"));
        assert!(resolve_discard_unreflected(Some(&settings), "other"));
    }
}
