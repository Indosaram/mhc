//! Typed view of the resolved `task` settings the lifecycle reads (`OmoTaskSettings` in TS).
//!
//! Private adapter: `omo-config-core` exposes only a JSON schema resolver, not a typed struct.

use omo_config_core::Issues;
use omo_config_core::schema::resolve_omo_task_settings_default;
use serde_json::Value;

/// `residency_max_children`: a positive cap or `"unlimited"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResidencyCap {
    Limited(usize),
    Unlimited,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TaskSettings {
    pub residency_max_children: ResidencyCap,
    pub ttl_ms: i64,
    pub reattach_on_reconcile: Option<bool>,
    pub resume_children: bool,
}

impl TaskSettings {
    /// Parses raw `task` settings through the schema (defaults applied) into the typed view.
    pub fn resolve(input: &Value) -> Result<Self, Issues> {
        resolve_omo_task_settings_default(input).map(|resolved| Self::from_resolved(&resolved))
    }

    /// Reads an already-resolved settings object; absent keys fall back to schema defaults.
    pub fn from_resolved(resolved: &Value) -> Self {
        let residency_max_children = match resolved.get("residency_max_children") {
            Some(Value::String(text)) if text == "unlimited" => ResidencyCap::Unlimited,
            Some(value) => value
                .as_u64()
                .and_then(|cap| usize::try_from(cap).ok())
                .map_or(ResidencyCap::Limited(8), ResidencyCap::Limited),
            None => ResidencyCap::Limited(8),
        };
        Self {
            residency_max_children,
            ttl_ms: resolved
                .get("ttl_ms")
                .and_then(Value::as_i64)
                .unwrap_or(86_400_000),
            reattach_on_reconcile: resolved
                .get("reattach_on_reconcile")
                .and_then(Value::as_bool),
            resume_children: resolved
                .get("resume_children")
                .and_then(Value::as_bool)
                .unwrap_or(true),
        }
    }
}
