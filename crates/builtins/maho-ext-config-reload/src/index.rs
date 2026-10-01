use std::collections::BTreeMap;
use serde_json::Value;

pub struct ConfigReloadHandoffRegistry<T> { handoffs: BTreeMap<String, T> }
impl<T> Default for ConfigReloadHandoffRegistry<T> { fn default() -> Self { Self { handoffs: BTreeMap::new() } } }
impl<T> ConfigReloadHandoffRegistry<T> {
    pub fn set(&mut self, session_handle: String, handoff: T) { self.handoffs.insert(session_handle, handoff); }
    pub fn take(&mut self, session_handle: &str) -> Option<T> { self.handoffs.remove(session_handle) }
    pub fn delete(&mut self, session_handle: &str) { self.handoffs.remove(session_handle); }
}
pub struct ResolvedConfigReloadSettings { pub enabled: bool, pub debounce_ms: f64, pub watch: BTreeMap<String, bool> }
pub fn resolve_config_reload_settings(global: &Value, project: &Value) -> ResolvedConfigReloadSettings {
    let global = global.get("configReload").filter(|value| value.is_object());
    let project = project.get("configReload").filter(|value| value.is_object());
    let enabled = project.and_then(|patch| patch.get("enabled")).and_then(Value::as_bool).or_else(|| global.and_then(|patch| patch.get("enabled")).and_then(Value::as_bool)).unwrap_or(true);
    let debounce = |patch: Option<&Value>| patch.and_then(|patch| patch.get("debounceMs")).and_then(Value::as_f64).filter(|value| value.is_finite() && *value >= 0.0).map(f64::floor);
    let watch = ["settings", "models", "keybindings", "prompts", "skills", "extensions"].into_iter().map(|key| {
        let field = |patch: Option<&Value>| patch.and_then(|patch| patch.get("watch")).and_then(|watch| watch.get(key)).and_then(Value::as_bool);
        (key.into(), field(project).or_else(|| field(global)).unwrap_or(true))
    }).collect();
    ResolvedConfigReloadSettings { enabled, debounce_ms: debounce(project).or_else(|| debounce(global)).unwrap_or(200.0), watch }
}
