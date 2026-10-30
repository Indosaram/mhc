//! Port of senpi packages/coding-agent/src/core/engine-socket-flag.ts.

use std::collections::HashMap;

/// The engine-socket compact-updates rollout flag (`<brand>_ENGINE_SOCKET_COMPACT_UPDATES`), explicit-true only.
/// Off, no session mints recovery identities or owns an acceptance lane, and every destination keeps its bytes.
pub fn compact_updates_enabled() -> bool {
    compact_updates_enabled_in(&crate::config::current_env())
}

pub fn compact_updates_enabled_in(env: &HashMap<String, String>) -> bool {
    matches!(
        crate::brand::env_value("ENGINE_SOCKET_COMPACT_UPDATES", env).as_deref(),
        Some("1" | "true")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_an_explicit_one_or_true_enables_compact_updates() {
        let name = crate::brand::brand_env_names("ENGINE_SOCKET_COMPACT_UPDATES", None)
            .into_iter()
            .next()
            .expect("brand env name");
        let with = |value: &str| HashMap::from([(name.clone(), value.to_owned())]);
        assert!(!compact_updates_enabled_in(&HashMap::new()));
        assert!(compact_updates_enabled_in(&with("1")));
        assert!(compact_updates_enabled_in(&with("true")));
        for off in ["", "0", "TRUE", "yes", "false"] {
            assert!(!compact_updates_enabled_in(&with(off)), "{off}");
        }
    }
}
