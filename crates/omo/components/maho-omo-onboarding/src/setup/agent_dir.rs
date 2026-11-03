//! Where omo keeps engine state (upstream `omo-native/bin/lib/agent-dir.js`, the part the setup
//! stream reads: `canonicalAgentDir`, `defaultAgentDir`, `runtimeHome`). The legacy flat-layout
//! adoption runs in the launcher, not in setup, and is not part of this port.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Env names carrying an explicit agent-state directory, most specific first.
pub const AGENT_DIR_ENV_NAMES: [&str; 3] = [
    "OMO_CODING_AGENT_DIR",
    "SENPI_CODING_AGENT_DIR",
    "PI_CODING_AGENT_DIR",
];

/// Windows launches carry the runtime home in the environment, which must outrank the OS home.
pub fn runtime_home(env: &BTreeMap<String, String>) -> String {
    env.get("HOME")
        .filter(|v| !v.is_empty())
        .or_else(|| env.get("USERPROFILE").filter(|v| !v.is_empty()))
        .cloned()
        .unwrap_or_else(crate::session_history::project_paths::default_home_dir)
}

/// Where omo keeps engine state: an explicit override, otherwise the canonical branded location.
pub fn canonical_agent_dir(env: &BTreeMap<String, String>, home: &str) -> PathBuf {
    for name in AGENT_DIR_ENV_NAMES {
        if let Some(configured) = env.get(name).map(|v| v.trim()).filter(|v| !v.is_empty()) {
            return std::path::absolute(configured).unwrap_or_else(|_| PathBuf::from(configured));
        }
    }
    default_agent_dir(Path::new(home))
}

pub fn default_agent_dir(home: &Path) -> PathBuf {
    home.join(".omo").join("agent")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_explicit_directory_wins_in_order_and_blank_values_are_unset() {
        // Overrides go through `resolve`, so they are absolute on this platform, as upstream's are.
        let root = std::env::temp_dir();
        let home = root.join("h");
        let home = home.to_str().expect("utf-8 temp dir");
        let mut env = BTreeMap::new();
        assert_eq!(
            canonical_agent_dir(&env, home),
            Path::new(home).join(".omo").join("agent")
        );
        env.insert(
            "PI_CODING_AGENT_DIR".into(),
            root.join("pi").to_string_lossy().into_owned(),
        );
        env.insert("SENPI_CODING_AGENT_DIR".into(), "  ".into());
        assert_eq!(canonical_agent_dir(&env, home), root.join("pi"));
        env.insert(
            "OMO_CODING_AGENT_DIR".into(),
            root.join("omo").to_string_lossy().into_owned(),
        );
        assert_eq!(canonical_agent_dir(&env, home), root.join("omo"));
    }
}
