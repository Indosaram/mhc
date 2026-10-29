use serde_json::Value;

pub fn resolve_plugin_path(path: &str, plugin_root: &str) -> String {
    path.replace("${CLAUDE_PLUGIN_ROOT}", plugin_root)
}

pub fn resolve_plugin_paths(value: &Value, plugin_root: &str) -> Value {
    match value {
        Value::String(text) => Value::String(resolve_plugin_path(text, plugin_root)),
        Value::Array(items) => Value::Array(items.iter().map(|item| resolve_plugin_paths(item, plugin_root)).collect()),
        Value::Object(object) => Value::Object(object.iter().map(|(key, value)| (key.clone(), resolve_plugin_paths(value, plugin_root))).collect()),
        Value::Null | Value::Bool(_) | Value::Number(_) => value.clone(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use super::*;
    #[test]
    fn replaces_all_placeholders_and_recurses_through_values() {
        assert_eq!(resolve_plugin_path("${CLAUDE_PLUGIN_ROOT}/dist/index.js", "/tmp/plugin-root"), "/tmp/plugin-root/dist/index.js");
        assert_eq!(resolve_plugin_path("${CLAUDE_PLUGIN_ROOT}/a ${CLAUDE_PLUGIN_ROOT}/b", "/root"), "/root/a /root/b");
        assert_eq!(resolve_plugin_paths(&json!({"command":"node","args":["${CLAUDE_PLUGIN_ROOT}/server.js"],"nested":{"config":"${CLAUDE_PLUGIN_ROOT}/config.json"},"null":null}), "/root"), json!({"command":"node","args":["/root/server.js"],"nested":{"config":"/root/config.json"},"null":null}));
        assert_eq!(resolve_plugin_paths(&Value::Null, "/root"), Value::Null);
    }
}
