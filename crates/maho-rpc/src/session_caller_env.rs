//! Port of senpi `packages/coding-agent/src/modes/rpc/session-caller-env.ts`.
//!
//! The per-spawn caller env channel (senpi#4381): the small, explicit set of environment
//! variables a caller may hand to the ONE session it opens, scoped to that session's tool
//! subprocesses and to the task children opened with `spawnedBy` naming it.

use crate::host_daemon_env::is_transient_daemon_env_name;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

pub type SessionEnv = BTreeMap<String, String>;

pub const CALLER_ENV_NAMES_ENV: &str = "SENPI_RPC_CALLER_ENV_NAMES";
pub const DEFAULT_CALLER_ENV_NAMES: [&str; 1] = ["OMOCAT_LANE"];

const MAX_SESSION_ENV_ENTRIES: usize = 16;
const MAX_SESSION_ENV_VALUE_LENGTH: usize = 4096;
const MAX_ECHOED_NAME_LENGTH: usize = 64;

fn is_valid_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_ascii_uppercase() || first == '_')
        && chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

fn echoed_name(name: &str) -> String {
    if name.len() <= MAX_ECHOED_NAME_LENGTH {
        name.to_owned()
    } else {
        let end = name
            .char_indices()
            .take_while(|(i, _)| *i < MAX_ECHOED_NAME_LENGTH)
            .last()
            .map_or(0, |(i, c)| i + c.len_utf8());
        format!("{}...", &name[..end])
    }
}

pub fn caller_env_passable_names(
    host_env: &std::collections::HashMap<String, String>,
) -> BTreeSet<String> {
    let mut names: BTreeSet<String> = DEFAULT_CALLER_ENV_NAMES
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
    let extra = maho_core::brand::env_value("RPC_CALLER_ENV_NAMES", host_env)
        .or_else(|| host_env.get(CALLER_ENV_NAMES_ENV).cloned())
        .unwrap_or_default();
    for raw in extra.split(',') {
        let name = raw.trim();
        if is_valid_env_name(name) && !is_transient_daemon_env_name(name) {
            names.insert(name.to_owned());
        }
    }
    names
}

pub fn session_env_shape_error(value: Option<&Value>) -> Option<String> {
    let value = value?;
    if value.is_null() {
        return Some("env must be an object of strings.".to_owned());
    }
    let Some(obj) = value.as_object() else {
        return Some("env must be an object of strings.".to_owned());
    };
    if obj.len() > MAX_SESSION_ENV_ENTRIES {
        return Some(format!(
            "env allows at most {MAX_SESSION_ENV_ENTRIES} entries."
        ));
    }
    for (name, entry) in obj {
        if !is_valid_env_name(name) {
            return Some(format!(
                "env name \"{}\" must match ^[A-Z_][A-Z0-9_]*$.",
                echoed_name(name)
            ));
        }
        let Some(text) = entry.as_str() else {
            return Some(format!("env.{} must be a string.", echoed_name(name)));
        };
        if text.contains('\0') {
            return Some(format!("env.{} must not contain NUL.", echoed_name(name)));
        }
        if text.len() > MAX_SESSION_ENV_VALUE_LENGTH {
            return Some(format!(
                "env.{} must be at most {MAX_SESSION_ENV_VALUE_LENGTH} characters.",
                echoed_name(name)
            ));
        }
    }
    None
}

pub fn session_env_error(
    value: Option<&Value>,
    host_env: &std::collections::HashMap<String, String>,
) -> Option<String> {
    if let Some(err) = session_env_shape_error(value) {
        return Some(err);
    }
    let obj = value?.as_object()?;
    let passable = caller_env_passable_names(host_env);
    for name in obj.keys() {
        if !passable.contains(name) {
            let list = passable.into_iter().collect::<Vec<_>>().join(", ");
            return Some(format!(
                "env.{} is not a caller-passable name (passable: {list}).",
                echoed_name(name)
            ));
        }
    }
    None
}

pub fn inherited_session_env(
    parent: Option<&SessionEnv>,
    requested: Option<&SessionEnv>,
) -> Option<SessionEnv> {
    let mut merged = SessionEnv::new();
    if let Some(parent) = parent {
        for (k, v) in parent {
            merged.insert(k.clone(), v.clone());
        }
    }
    if let Some(requested) = requested {
        for (k, v) in requested {
            merged.insert(k.clone(), v.clone());
        }
    }
    if merged.is_empty() {
        None
    } else {
        Some(merged)
    }
}

pub fn parse_session_env(value: Option<&Value>) -> Option<SessionEnv> {
    let obj = value?.as_object()?;
    let mut out = SessionEnv::new();
    for (k, v) in obj {
        if let Some(s) = v.as_str() {
            out.insert(k.clone(), s.to_owned());
        }
    }
    if out.is_empty() { None } else { Some(out) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::HashMap;

    #[test]
    fn validates_caller_env_names_and_inherits() {
        let host_env = HashMap::from([(
            CALLER_ENV_NAMES_ENV.to_owned(),
            "CUSTOM_VAR,SENPI_SESSION_ID".to_owned(),
        )]);
        let passable = caller_env_passable_names(&host_env);
        assert!(passable.contains("OMOCAT_LANE"));
        assert!(passable.contains("CUSTOM_VAR"));
        assert!(!passable.contains("SENPI_SESSION_ID"));

        assert_eq!(
            session_env_error(Some(&json!({"OMOCAT_LANE": "alpha"})), &host_env),
            None
        );
        assert!(
            session_env_error(Some(&json!({"SECRET": "x"})), &host_env)
                .is_some_and(|e| e.contains("not a caller-passable name"))
        );

        let parent = BTreeMap::from([
            ("OMOCAT_LANE".to_owned(), "p".to_owned()),
            ("CUSTOM_VAR".to_owned(), "1".to_owned()),
        ]);
        let child = BTreeMap::from([("OMOCAT_LANE".to_owned(), "c".to_owned())]);
        let merged = inherited_session_env(Some(&parent), Some(&child)).expect("merged");
        assert_eq!(merged["OMOCAT_LANE"], "c");
        assert_eq!(merged["CUSTOM_VAR"], "1");
    }
}
