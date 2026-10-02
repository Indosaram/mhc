use std::collections::{BTreeMap, BTreeSet};
use super::types::{LoadedRule, RuleDiagnostic};

#[derive(Default, Debug)]
pub struct SessionState {
    pub cwd: Option<String>,
    pub static_dedup: BTreeSet<String>,
    pub dynamic_dedup: BTreeMap<String, BTreeSet<String>>,
    pub dynamic_target_fingerprints: BTreeMap<String, String>,
    pub loaded_rules: Vec<LoadedRule>,
    pub diagnostics: Vec<RuleDiagnostic>,
}
pub fn create_session_state(cwd: Option<String>) -> SessionState { SessionState { cwd, ..SessionState::default() } }
pub fn static_dedup_key(cwd: &str, rule_path: &str, content_hash: &str) -> String { format!("{cwd}::{rule_path}::{content_hash}") }
pub fn dynamic_dedup_key(scope_key: &str, rule_path: &str, content_hash: &str) -> String { static_dedup_key(scope_key, rule_path, content_hash) }
pub fn mark_static_injected(state: &mut SessionState, rule: &LoadedRule) -> bool {
    state.static_dedup.insert(static_dedup_key(state.cwd.as_deref().unwrap_or(""), &rule.candidate.real_path, &rule.content_hash))
}
pub fn mark_dynamic_injected(state: &mut SessionState, scope_key: &str, rule: &LoadedRule) -> bool {
    state.dynamic_dedup.entry(scope_key.into()).or_default().insert(dynamic_dedup_key(scope_key, &rule.candidate.real_path, &rule.content_hash))
}
pub fn is_static_injected(state: &SessionState, rule: &LoadedRule) -> bool {
    state.static_dedup.contains(&static_dedup_key(state.cwd.as_deref().unwrap_or(""), &rule.candidate.real_path, &rule.content_hash))
}
pub fn is_dynamic_injected(state: &SessionState, scope_key: &str, rule: &LoadedRule) -> bool {
    state.dynamic_dedup.get(scope_key).is_some_and(|keys| keys.contains(&dynamic_dedup_key(scope_key, &rule.candidate.real_path, &rule.content_hash)))
}
pub fn clear_session(state: &mut SessionState) {
    state.static_dedup.clear(); state.dynamic_dedup.clear(); state.dynamic_target_fingerprints.clear(); state.loaded_rules.clear(); state.diagnostics.clear();
}
