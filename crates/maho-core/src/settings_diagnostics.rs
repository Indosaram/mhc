//! Port of senpi packages/coding-agent/src/core/settings-diagnostics.ts.

use std::collections::BTreeSet;

use serde_json::Value;

use crate::settings_manager::{SettingsManager, SettingsScope};

/// A startup/runtime diagnostic surfaced by the settings collectors. senpi declares this in
/// agent-session-services.ts; it is defined here and re-exported from there to keep the module
/// graph acyclic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentSessionRuntimeDiagnostic {
    pub kind: String,
    pub message: String,
}

impl AgentSessionRuntimeDiagnostic {
    pub fn warning(message: impl Into<String>) -> Self {
        Self { kind: "warning".to_owned(), message: message.into() }
    }
}

fn scope_label(scope: SettingsScope) -> &'static str {
    match scope {
        SettingsScope::Global => "global",
        SettingsScope::Project => "project",
    }
}

fn provider_settings_warnings(settings_manager: &SettingsManager) -> Vec<String> {
    let mut warnings = Vec::new();
    let Some(providers) = settings_manager.get().get("providers").and_then(Value::as_object) else { return warnings };
    for (provider_id, settings) in providers {
        let Some(value) = settings.get("maxConcurrency") else { continue };
        let valid = value.as_i64().map(|number| number >= 0).unwrap_or(false);
        if !valid {
            warnings.push(format!(
                "Invalid providers.{provider_id}.maxConcurrency: expected a non-negative integer; using unlimited"
            ));
        }
    }
    warnings
}

pub fn collect_settings_diagnostics(settings_manager: &SettingsManager) -> Vec<AgentSessionRuntimeDiagnostic> {
    let mut diagnostics: Vec<AgentSessionRuntimeDiagnostic> = settings_manager
        .errors()
        .iter()
        .map(|error| {
            AgentSessionRuntimeDiagnostic::warning(match &error.path {
                Some(path) => format!("Invalid settings file {path}: {}", error.error),
                None => format!("Invalid {} settings: {}", scope_label(error.scope), error.error),
            })
        })
        .collect();
    diagnostics.extend(provider_settings_warnings(settings_manager).into_iter().map(AgentSessionRuntimeDiagnostic::warning));
    diagnostics
}

/// The CLI labels every diagnostic with the startup phase that produced it; it shares this module's
/// validation so a settings check can never reach one collector and miss the other.
pub fn collect_settings_diagnostics_with_context(
    settings_manager: &SettingsManager,
    context: &str,
) -> Vec<AgentSessionRuntimeDiagnostic> {
    let mut diagnostics: Vec<AgentSessionRuntimeDiagnostic> = settings_manager
        .errors()
        .iter()
        .map(|error| {
            AgentSessionRuntimeDiagnostic::warning(format!(
                "({context}, {} settings) {}",
                scope_label(error.scope),
                error.error
            ))
        })
        .collect();
    diagnostics.extend(
        provider_settings_warnings(settings_manager)
            .into_iter()
            .map(|message| AgentSessionRuntimeDiagnostic::warning(format!("({context}) {message}"))),
    );
    diagnostics
}

/// Removes duplicate type/message diagnostics while preserving their first occurrence.
pub fn deduplicate_diagnostics(
    diagnostics: &[AgentSessionRuntimeDiagnostic],
) -> Vec<AgentSessionRuntimeDiagnostic> {
    let mut seen = BTreeSet::new();
    diagnostics
        .iter()
        .filter(|diagnostic| seen.insert(format!("{}\0{}", diagnostic.kind, diagnostic.message)))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings_manager::InMemorySettingsStorage;
    use serde_json::json;

    fn settings() -> SettingsManager {
        SettingsManager::from_storage(Box::new(InMemorySettingsStorage::default()), true)
    }

    #[test]
    fn a_valid_concurrency_setting_produces_no_warning() {
        let mut manager = settings();
        let mut values = crate::settings_manager::Settings::new();
        values.insert("providers".into(), json!({ "p": { "maxConcurrency": 3 } }));
        manager.set(SettingsScope::Global, &values).expect("set");
        assert!(collect_settings_diagnostics(&manager).is_empty());
    }

    #[test]
    fn an_invalid_concurrency_setting_warns() {
        let mut manager = settings();
        let mut values = crate::settings_manager::Settings::new();
        values.insert("providers".into(), json!({ "p": { "maxConcurrency": -1 } }));
        manager.set(SettingsScope::Global, &values).expect("set");
        let diagnostics = collect_settings_diagnostics(&manager);
        assert_eq!(diagnostics.len(), 1);
        assert!(diagnostics[0].message.contains("maxConcurrency"));
    }

    #[test]
    fn the_context_collector_prefixes_the_phase() {
        let mut manager = settings();
        let mut values = crate::settings_manager::Settings::new();
        values.insert("providers".into(), json!({ "p": { "maxConcurrency": "x" } }));
        manager.set(SettingsScope::Global, &values).expect("set");
        let diagnostics = collect_settings_diagnostics_with_context(&manager, "startup");
        assert!(diagnostics[0].message.starts_with("(startup)"));
    }

    #[test]
    fn deduplication_keeps_the_first_occurrence() {
        let diagnostics = vec![
            AgentSessionRuntimeDiagnostic::warning("a"),
            AgentSessionRuntimeDiagnostic::warning("a"),
            AgentSessionRuntimeDiagnostic::warning("b"),
        ];
        assert_eq!(deduplicate_diagnostics(&diagnostics).len(), 2);
    }
}
