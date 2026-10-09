//! Spawn role guard for LazyCodex agent routing.

use std::collections::BTreeSet;
use serde_json::Value;

pub const LAZYCODEX_SPAWN_ROLES: [&str; 12] = [
    "explorer",
    "lazycodex-clone-fidelity-reviewer",
    "lazycodex-code-reviewer",
    "lazycodex-gate-reviewer",
    "lazycodex-qa-executor",
    "lazycodex-worker-high",
    "lazycodex-worker-low",
    "lazycodex-worker-medium",
    "librarian",
    "metis",
    "momus",
    "plan",
];

pub fn spawn_role_denial<F>(input: &Value, registered_roles: F) -> Option<String>
where
    F: FnOnce() -> BTreeSet<String>,
{
    let role = input.as_object().and_then(|o| o.get("agent_type")).and_then(|v| v.as_str());
    if let Some(r) = role {
        if LAZYCODEX_SPAWN_ROLES.contains(&r) {
            return None;
        }
    }

    let registered = if let Some(r) = role {
        if !r.trim().is_empty() {
            registered_roles()
        } else {
            BTreeSet::new()
        }
    } else {
        BTreeSet::new()
    };

    if let Some(r) = role {
        if registered.contains(r) {
            return None;
        }
    }

    let mut known: Vec<String> = LAZYCODEX_SPAWN_ROLES.iter().map(|s| s.to_string()).collect();
    for r in registered {
        if !known.contains(&r) {
            known.push(r);
        }
    }

    let role_display = role.map(|r| format!("\"{r}\"")).unwrap_or_else(|| "no agent_type".to_string());
    Some(format!(
        "LazyCodex requires an explicit registered agent_type: {}. Received {role_display}. Use the matching role and fork_turns: \"none\" (V2) or fork_context: false (V1), unless full history is deliberately required. The hook cannot see the tool schema; if agent_type is unavailable, stop and report incompatible role routing rather than spawning a generic agent. Describing a role in message does not select its TOML.",
        known.join(", ")
    ))
}
