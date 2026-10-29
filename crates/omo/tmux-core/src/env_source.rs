//! Read-only environment lookup, injectable so callers and tests never mutate the process env.

use std::collections::HashMap;

/// A source of environment variables (`process.env` in the TypeScript original).
pub trait EnvSource: Send + Sync {
    fn var(&self, key: &str) -> Option<String>;
}

/// The live process environment.
#[derive(Debug, Clone, Copy, Default)]
pub struct ProcessEnv;

impl EnvSource for ProcessEnv {
    fn var(&self, key: &str) -> Option<String> {
        std::env::var(key).ok()
    }
}

impl EnvSource for HashMap<String, String> {
    fn var(&self, key: &str) -> Option<String> {
        self.get(key).cloned()
    }
}

impl<const N: usize> EnvSource for [(&'static str, &'static str); N] {
    fn var(&self, key: &str) -> Option<String> {
        self.iter()
            .find(|(name, _)| *name == key)
            .map(|(_, value)| (*value).to_owned())
    }
}

/// JavaScript truthiness of an optional env value: set and non-empty.
pub(crate) fn is_set(value: Option<&str>) -> bool {
    value.is_some_and(|value| !value.is_empty())
}
