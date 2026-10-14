//! Process-environment writes for the CLI entry point.
//!
//! senpi assigns `process.env` at startup (`cli.ts`, `cli-main.ts`, `main.ts`): the agent markers,
//! the brand scrub, offline mode and the HTTP proxy. Child processes and HTTP clients read the real
//! process environment, so an in-memory overlay cannot reproduce that. Edition 2024 makes
//! `std::env::set_var` unsafe and the workspace forbids unsafe code; this edition-2021 crate keeps
//! the calls safe. The caller must apply updates only while the process is single-threaded.

use std::collections::HashMap;

/// Make the process environment equal `next`, given the `current` snapshot: remove the keys that are
/// absent from `next`, and set the keys that are new or have changed.
pub fn apply(current: &HashMap<String, String>, next: &HashMap<String, String>) {
    for key in current.keys() {
        if !next.contains_key(key) {
            std::env::remove_var(key);
        }
    }
    for (key, value) in next {
        if current.get(key) != Some(value) {
            std::env::set_var(key, value);
        }
    }
}
