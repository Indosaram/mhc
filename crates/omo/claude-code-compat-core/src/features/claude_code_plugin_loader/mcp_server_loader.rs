use super::{plugin_path_resolver::resolve_plugin_paths, types::LoadedPlugin};
use crate::features::claude_code_mcp_loader::{
    env_expander::{ExpandEnvVarsOptions, expand_env_vars_in_object},
    scope_filter::should_load_mcp_server,
    transformer::transform_mcp_server,
    types::{ClaudeCodeMcpConfig, McpServerConfig},
};
use crate::shared::logger::log;
use serde_json::Value;
use std::{collections::BTreeMap, fs, io};

pub fn load_plugin_mcp_servers_sync(
    plugins: &[LoadedPlugin],
) -> io::Result<BTreeMap<String, McpServerConfig>> {
    let cwd = std::env::current_dir()?;
    load_plugin_mcp_servers_for_cwd(plugins, &cwd)
}

fn load_plugin_mcp_servers_for_cwd(
    plugins: &[LoadedPlugin],
    cwd: &std::path::Path,
) -> io::Result<BTreeMap<String, McpServerConfig>> {
    let mut servers = BTreeMap::new();

    for plugin in plugins {
        let Some(mcp_path) = plugin.mcp_path.as_ref().filter(|path| path.exists()) else {
            continue;
        };

        let content = match fs::read_to_string(mcp_path) {
            Ok(content) => content,
            Err(error) => {
                log(
                    &format!(
                        "Failed to load plugin MCP config: {}: {error}",
                        mcp_path.display()
                    ),
                    None,
                );
                continue;
            }
        };

        let config_val: Value = match serde_json::from_str(&content) {
            Ok(val) => val,
            Err(error) => {
                log(
                    &format!(
                        "Failed to load plugin MCP config: {}: {error}",
                        mcp_path.display()
                    ),
                    None,
                );
                continue;
            }
        };

        let root = plugin.install_path.to_string_lossy();
        let config_val = resolve_plugin_paths(&config_val, &root);
        let config_val = expand_env_vars_in_object(&config_val, ExpandEnvVarsOptions::default());

        let config: ClaudeCodeMcpConfig = match serde_json::from_value(config_val) {
            Ok(config) => config,
            Err(error) => {
                log(
                    &format!(
                        "Failed to load plugin MCP config: {}: {error}",
                        mcp_path.display()
                    ),
                    None,
                );
                continue;
            }
        };

        for (name, server_config) in config.mcp_servers {
            let should_load = match should_load_mcp_server(&server_config, Some(cwd)) {
                Ok(should) => should,
                Err(error) => {
                    log(
                        &format!(
                            "Failed to evaluate scope for plugin MCP server \"{name}\": {error}"
                        ),
                        None,
                    );
                    continue;
                }
            };

            if !should_load {
                log(
                    &format!("Skipping local plugin MCP server \"{name}\" outside current cwd"),
                    None,
                );
                continue;
            }

            if server_config.disabled == Some(true) {
                log(
                    &format!(
                        "Skipping disabled MCP server \"{name}\" from plugin {}",
                        plugin.name
                    ),
                    None,
                );
                continue;
            }

            match transform_mcp_server(&name, &server_config) {
                Ok(transformed) => {
                    let namespaced_name = format!("{}:{}", plugin.name, name);
                    servers.insert(namespaced_name.clone(), transformed);
                    log(
                        &format!("Loaded plugin MCP server: {namespaced_name}"),
                        None,
                    );
                }
                Err(error) => {
                    log(
                        &format!("Failed to transform plugin MCP server \"{name}\": {error}"),
                        None,
                    );
                }
            }
        }
    }

    Ok(servers)
}

pub async fn load_plugin_mcp_servers(
    plugins: &[LoadedPlugin],
) -> io::Result<BTreeMap<String, McpServerConfig>> {
    load_plugin_mcp_servers_sync(plugins)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::claude_code_plugin_loader::types::PluginScope;
    use serde_json::json;
    use std::fs;

    #[test]
    fn local_scope_filters_servers_by_current_working_directory() {
        let _guard = crate::TEST_PROCESS_STATE
            .lock()
            .expect("process state lock");
        let root = tempfile::tempdir().expect("fixture directory");
        let project_dir = root.path().join("project");
        let project_subdir = project_dir.join("packages/app");
        let plugin_dir = root.path().join("plugin");
        fs::create_dir_all(&project_subdir).expect("project subdir");
        fs::create_dir_all(&plugin_dir).expect("plugin dir");

        let mcp_path = plugin_dir.join("mcp.json");
        let mcp_content = json!({
            "mcpServers": {
                "globalServer": {
                    "command": "npx",
                    "args": ["global-plugin-server"]
                },
                "matchingLocal": {
                    "command": "npx",
                    "args": ["matching-plugin-local"],
                    "scope": "local",
                    "projectPath": project_dir.to_string_lossy()
                },
                "nonMatchingLocal": {
                    "command": "npx",
                    "args": ["non-matching-plugin-local"],
                    "scope": "local",
                    "projectPath": root.path().join("other-project").to_string_lossy()
                },
                "parentLocal": {
                    "command": "npx",
                    "args": ["parent-plugin-local"],
                    "scope": "local",
                    "projectPath": project_subdir.join("nested-project").to_string_lossy()
                },
                "disabledServer": {
                    "command": "npx",
                    "args": ["disabled-server"],
                    "disabled": true
                }
            }
        });
        fs::write(&mcp_path, mcp_content.to_string()).expect("write mcp json");

        let plugin = LoadedPlugin {
            name: "demo-plugin".to_string(),
            version: "1.0.0".to_string(),
            scope: PluginScope::Project,
            install_path: plugin_dir,
            manifest: None,
            plugin_key: "demo-plugin@test".to_string(),
            commands_dir: None,
            agents_dir: None,
            skills_dir: None,
            hooks_path: None,
            mcp_path: Some(mcp_path),
            lsp_path: None,
        };

        let servers = load_plugin_mcp_servers_for_cwd(&[plugin], &project_subdir).expect("servers");

        assert!(servers.contains_key("demo-plugin:globalServer"));
        assert!(servers.contains_key("demo-plugin:matchingLocal"));
        assert!(!servers.contains_key("demo-plugin:nonMatchingLocal"));
        assert!(!servers.contains_key("demo-plugin:parentLocal"));
        assert!(!servers.contains_key("demo-plugin:disabledServer"));
    }

    #[test]
    fn resolves_plugin_root_in_server_config() {
        let root = tempfile::tempdir().expect("fixture directory");
        let plugin_dir = root.path().join("path-plugin");
        fs::create_dir_all(&plugin_dir).expect("plugin dir");

        let mcp_path = plugin_dir.join("mcp.json");
        let mcp_content = json!({
            "mcpServers": {
                "server": {
                    "command": "node",
                    "args": ["${CLAUDE_PLUGIN_ROOT}/dist/index.js"]
                }
            }
        });
        fs::write(&mcp_path, mcp_content.to_string()).expect("write mcp json");

        let plugin = LoadedPlugin {
            name: "path-plugin".to_string(),
            version: "1.0.0".to_string(),
            scope: PluginScope::User,
            install_path: plugin_dir.clone(),
            manifest: None,
            plugin_key: "path-plugin@test".to_string(),
            commands_dir: None,
            agents_dir: None,
            skills_dir: None,
            hooks_path: None,
            mcp_path: Some(mcp_path),
            lsp_path: None,
        };

        let servers = load_plugin_mcp_servers_sync(&[plugin]).expect("load servers");
        let server = servers.get("path-plugin:server").expect("server");
        match server {
            McpServerConfig::Local { command, .. } => {
                assert_eq!(
                    command[1],
                    format!("{}/dist/index.js", plugin_dir.display())
                );
            }
            _ => panic!("expected local config"),
        }
    }
}
