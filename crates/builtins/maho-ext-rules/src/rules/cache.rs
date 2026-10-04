use super::types::{LoadedRule, SessionState};
pub fn create_session_state(cwd: Option<String>) -> SessionState { SessionState { cwd, ..Default::default() } }
pub fn static_dedup_key(cwd: &str, rule_path: &str, hash: &str) -> String { format!("{cwd}::{rule_path}::{hash}") }
pub fn dynamic_dedup_key(scope: &str, rule_path: &str, hash: &str) -> String { format!("{scope}::{rule_path}::{hash}") }
pub fn mark_static_injected(state: &mut SessionState, rule: &LoadedRule) -> bool { state.static_dedup.insert(static_dedup_key(state.cwd.as_deref().unwrap_or(""), &rule.candidate.real_path, &rule.content_hash)) }
pub fn mark_dynamic_injected(state: &mut SessionState, scope: &str, rule: &LoadedRule) -> bool { state.dynamic_dedup.entry(scope.into()).or_default().insert(dynamic_dedup_key(scope, &rule.candidate.real_path, &rule.content_hash)) }
pub fn is_static_injected(state: &SessionState, rule: &LoadedRule) -> bool { state.static_dedup.contains(&static_dedup_key(state.cwd.as_deref().unwrap_or(""), &rule.candidate.real_path, &rule.content_hash)) }
pub fn is_dynamic_injected(state: &SessionState, scope: &str, rule: &LoadedRule) -> bool { state.dynamic_dedup.get(scope).is_some_and(|keys| keys.contains(&dynamic_dedup_key(scope, &rule.candidate.real_path, &rule.content_hash))) }
pub fn clear_session(state: &mut SessionState) { state.static_dedup.clear(); state.dynamic_dedup.clear(); state.dynamic_target_fingerprints.clear(); state.loaded_rules.clear(); state.diagnostics.clear(); }
