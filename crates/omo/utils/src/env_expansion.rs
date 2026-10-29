//! `${VAR}` / `${VAR:-default}` expansion with an allow-list gate.

use std::collections::HashMap;
use std::sync::LazyLock;

use regex::{Captures, Regex};
use serde_json::{Map, Value};

use crate::deep_merge::is_unsafe_object_key;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvExpansionBlockedReason {
    NotAllowed,
}

impl EnvExpansionBlockedReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            EnvExpansionBlockedReason::NotAllowed => "not_allowed",
        }
    }
}

pub type EnvBlockedFn<'a> = &'a dyn Fn(&str, EnvExpansionBlockedReason);

#[derive(Default)]
pub struct EnvExpansionOptions<'a> {
    /// Environment to read from; `None` reads the process environment.
    pub env: Option<&'a HashMap<String, String>>,
    pub trusted: bool,
    pub is_allowed: Option<&'a dyn Fn(&str) -> bool>,
    pub on_blocked: Option<EnvBlockedFn<'a>>,
}

static ENV_REFERENCE_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\$\{([^}:]+)(?::-([^}]*))?\}").unwrap_or_else(|error| panic!("{error}"))
});

pub fn expand_env_references(value: &str, options: &EnvExpansionOptions<'_>) -> String {
    ENV_REFERENCE_PATTERN
        .replace_all(value, |captures: &Captures<'_>| {
            let name = &captures[1];
            let default_value = captures.get(2).map(|m| m.as_str());
            let allowed = options.is_allowed.is_none_or(|is_allowed| is_allowed(name));
            if !options.trusted && !allowed {
                if let Some(on_blocked) = options.on_blocked {
                    on_blocked(name, EnvExpansionBlockedReason::NotAllowed);
                }
                return default_value.unwrap_or_default().to_string();
            }
            let resolved = match options.env {
                Some(env) => env.get(name).cloned(),
                None => std::env::var(name).ok(),
            };
            resolved
                .or_else(|| default_value.map(str::to_string))
                .unwrap_or_default()
        })
        .into_owned()
}

pub fn expand_env_references_in_object(value: &Value, options: &EnvExpansionOptions<'_>) -> Value {
    match value {
        Value::String(text) => Value::String(expand_env_references(text, options)),
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| expand_env_references_in_object(item, options))
                .collect(),
        ),
        Value::Object(object) => {
            let mut result = Map::new();
            for (key, nested) in object {
                if is_unsafe_object_key(key) {
                    continue;
                }
                result.insert(
                    key.clone(),
                    expand_env_references_in_object(nested, options),
                );
            }
            Value::Object(result)
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => value.clone(),
    }
}
