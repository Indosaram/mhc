//! Load-time availability (spec §1): is an xAI credential connected?

use serde_json::Value;
use std::path::Path;

/// The credential file name inside the agent directory (interface fact).
pub const AUTH_FILE: &str = "auth.json";
/// The environment variable holding an xAI API key (interface fact).
pub const API_KEY_ENV: &str = "XAI_API_KEY";

/// Whether the `auth.json` content names a usable `xai` credential.
pub fn auth_file_connects(text: &str) -> bool {
    let Ok(Value::Object(root)) = serde_json::from_str::<Value>(text) else {
        return false;
    };
    root.get("xai")
        .and_then(Value::as_object)
        .and_then(|entry| entry.get("type"))
        .and_then(Value::as_str)
        .is_some_and(|kind| kind == "oauth" || kind == "api_key")
}

/// The trimmed `XAI_API_KEY`, when non-blank.
pub fn env_api_key(env: &dyn Fn(&str) -> Option<String>) -> Option<String> {
    env(API_KEY_ENV)
        .map(|key| key.trim().to_owned())
        .filter(|key| !key.is_empty())
}

/// Spec §1: an existing `auth.json` decides alone (fail closed on any read/parse/shape problem);
/// only without that file does a non-blank `XAI_API_KEY` connect.
pub fn is_connected(agent_dir: &Path, env: &dyn Fn(&str) -> Option<String>) -> bool {
    let path = agent_dir.join(AUTH_FILE);
    match std::fs::symlink_metadata(&path) {
        Ok(_) => std::fs::read_to_string(&path).is_ok_and(|text| auth_file_connects(&text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => env_api_key(env).is_some(),
        // Present but not even stat-able: treat as an unreadable file.
        Err(_) => false,
    }
}

/// Offline mode (`PI_OFFLINE` / `MAHO_OFFLINE` truthy) skips registration (spec §7, mcp.md).
pub fn is_offline(env: &dyn Fn(&str) -> Option<String>) -> bool {
    ["PI_OFFLINE", "MAHO_OFFLINE"].iter().any(|key| {
        env(key).is_some_and(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
    })
}
