use std::{collections::BTreeMap, fs};
use serde_json::Value;
use super::discovery_paths::get_claude_settings_path;

pub fn load_claude_settings() -> Option<Value> {
    let content = match fs::read_to_string(get_claude_settings_path()) { Ok(content) => content, Err(_) => return None };
    serde_json::from_str(&content).ok()
}

pub fn is_plugin_enabled(plugin_key: &str, settings: Option<&Value>, overrides: Option<&BTreeMap<String, bool>>) -> bool {
    overrides.and_then(|values| values.get(plugin_key).copied())
        .or_else(|| settings.and_then(|values| values.get(plugin_key)).and_then(Value::as_bool)).unwrap_or(true)
}
