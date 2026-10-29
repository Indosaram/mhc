use super::{
    plugin_path_resolver::resolve_plugin_paths,
    types::{HooksConfig, LoadedPlugin},
};
use crate::shared::logger::log;
use serde_json::Value;
use std::fs;

fn stamp_plugin_root(config: &mut HooksConfig, plugin_root: &str) {
    let Some(events) = config.get_mut("hooks").and_then(Value::as_object_mut) else {
        return;
    };
    for matchers in events.values_mut() {
        let Some(matchers) = matchers.as_array_mut() else {
            continue;
        };
        for matcher in matchers {
            let Some(actions) = matcher.get_mut("hooks").and_then(Value::as_array_mut) else {
                continue;
            };
            for action in actions {
                if matches!(
                    action.get("type").and_then(Value::as_str),
                    Some("command" | "http")
                ) && let Some(action) = action.as_object_mut()
                {
                    action.insert(
                        "pluginRoot".to_owned(),
                        Value::String(plugin_root.to_owned()),
                    );
                }
            }
        }
    }
}

pub fn load_plugin_hooks_configs(plugins: &[LoadedPlugin]) -> Vec<HooksConfig> {
    let mut configs = Vec::new();
    for plugin in plugins {
        let Some(path) = plugin.hooks_path.as_ref().filter(|path| path.exists()) else {
            continue;
        };
        let config: Value = match fs::read_to_string(path)
            .map_err(|e| e.to_string())
            .and_then(|content| serde_json::from_str(&content).map_err(|e| e.to_string()))
        {
            Ok(config) => config,
            Err(error) => {
                log(
                    &format!(
                        "Failed to load plugin hooks config: {}: {error}",
                        path.display()
                    ),
                    None,
                );
                continue;
            }
        };
        let root = plugin.install_path.to_string_lossy();
        let mut config = resolve_plugin_paths(&config, &root);
        stamp_plugin_root(&mut config, &root);
        configs.push(config);
    }
    configs
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn command_and_http_actions_receive_plugin_root_after_path_expansion() {
        let root = tempfile::tempdir().expect("fixture directory");
        let path = root.path().join("hooks.json");
        fs::write(&path, json!({"hooks": {
            "UserPromptSubmit": [{"matcher":"*", "hooks":[{"type":"command","command":"echo hi"}, {"type":"command","command":"${CLAUDE_PLUGIN_ROOT}/scripts/foo.sh"}]}],
            "PostToolUse": [{"matcher":"Bash", "hooks":[{"type":"http","url":"https://example.test/hook"}, {"type":"prompt","prompt":"value"}, {"type":"agent","agent":"test"}]}]
        }}).to_string()).expect("fixture write");
        let plugin = LoadedPlugin {
            name: "demo".into(),
            install_path: root.path().to_path_buf(),
            hooks_path: Some(path),
            ..Default::default()
        };
        let configs = load_plugin_hooks_configs(&[plugin]);
        assert_eq!(configs.len(), 1);
        for event in ["UserPromptSubmit", "PostToolUse"] {
            for action in configs[0]["hooks"][event][0]["hooks"]
                .as_array()
                .expect("actions")
            {
                let expected = matches!(action["type"].as_str(), Some("command" | "http"));
                assert_eq!(action.get("pluginRoot").is_some(), expected);
                if expected {
                    assert_eq!(action["pluginRoot"], root.path().to_string_lossy().as_ref());
                }
            }
        }
        assert_eq!(
            configs[0]["hooks"]["UserPromptSubmit"][0]["hooks"][1]["command"],
            format!("{}/scripts/foo.sh", root.path().display())
        );
    }
}
