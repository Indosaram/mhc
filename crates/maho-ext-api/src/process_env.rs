//! Process-environment writes made by extensions.
//!
//! Upstream extensions assign `process.env.X = value` or `delete process.env.X`, and every child the
//! agent spawns afterwards inherits that environment. Edition 2024 makes `std::env::set_var` unsafe
//! and the workspace forbids unsafe code, so the writes are recorded here and applied wherever a
//! child environment is built from the process environment.

use std::collections::BTreeMap;
use std::sync::{Mutex, PoisonError};

/// `Some(value)` sets the key, `None` deletes it.
static OVERLAY: Mutex<BTreeMap<String, Option<String>>> = Mutex::new(BTreeMap::new());

/// `process.env[key] = value`.
pub fn set(key: &str, value: &str) {
    OVERLAY.lock().unwrap_or_else(PoisonError::into_inner).insert(key.to_owned(), Some(value.to_owned()));
}

/// `delete process.env[key]`.
pub fn delete(key: &str) {
    OVERLAY.lock().unwrap_or_else(PoisonError::into_inner).insert(key.to_owned(), None);
}

/// `process.env[key]` after the recorded writes.
pub fn var(key: &str) -> Option<String> {
    match OVERLAY.lock().unwrap_or_else(PoisonError::into_inner).get(key) {
        Some(value) => value.clone(),
        None => std::env::var(key).ok(),
    }
}

/// Apply the recorded writes to a child environment built from the process environment.
pub fn apply(env: &mut BTreeMap<String, String>) {
    for (key, value) in OVERLAY.lock().unwrap_or_else(PoisonError::into_inner).iter() {
        match value {
            Some(value) => { env.insert(key.clone(), value.clone()); }
            None => { env.remove(key); }
        }
    }
}

/// The whole `process.env` after the recorded writes.
pub fn vars() -> BTreeMap<String, String> {
    let mut env = std::env::vars().collect();
    apply(&mut env);
    env
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_and_deletes_reach_child_environments() {
        let inherited = std::env::vars().map(|(key, _)| key).next().expect("an inherited variable");
        set("MAHO_PROCESS_ENV_OVERLAY_TEST", "1");
        delete(&inherited);
        let env = vars();
        assert_eq!(env.get("MAHO_PROCESS_ENV_OVERLAY_TEST").map(String::as_str), Some("1"));
        assert!(!env.contains_key(&inherited));
        assert_eq!(var(&inherited), None);
        assert_eq!(var("MAHO_PROCESS_ENV_OVERLAY_TEST").as_deref(), Some("1"));
    }
}
