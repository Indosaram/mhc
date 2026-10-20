use super::{scope_filter::should_load_mcp_server, transformer::transform_mcp_server, types::*};
use crate::shared::get_claude_config_dir;
use crate::shared::logger::log;
use std::{
    collections::BTreeSet,
    fs, io,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Default)]
pub struct McpLoaderOptions {
    pub cwd: Option<PathBuf>,
    pub home_dir: Option<PathBuf>,
    pub claude_config_dir: Option<PathBuf>,
}
#[derive(Debug, Clone)]
pub struct ResolvedMcpLoaderContext {
    pub cwd: PathBuf,
    pub home_dir: PathBuf,
    pub claude_config_dir: PathBuf,
}

pub fn resolve_mcp_loader_context(
    options: &McpLoaderOptions,
) -> io::Result<ResolvedMcpLoaderContext> {
    let cwd = match &options.cwd {
        Some(cwd) => cwd.clone(),
        None => std::env::current_dir()?,
    };
    let home_dir = options.home_dir.clone().unwrap_or_else(|| {
        std::env::var_os("HOME")
            .filter(|v| !v.is_empty())
            .or_else(|| std::env::var_os("USERPROFILE").filter(|v| !v.is_empty()))
            .map(PathBuf::from)
            .or_else(dirs::home_dir)
            .unwrap_or_default()
    });
    let claude_config_dir = options.claude_config_dir.clone().unwrap_or_else(|| {
        if options.home_dir.is_some() {
            home_dir.join(".claude")
        } else {
            get_claude_config_dir()
        }
    });
    Ok(ResolvedMcpLoaderContext {
        cwd,
        home_dir,
        claude_config_dir,
    })
}
fn get_mcp_config_paths(context: &ResolvedMcpLoaderContext) -> [(PathBuf, McpScope); 4] {
    [
        (context.home_dir.join(".claude.json"), McpScope::User),
        (context.claude_config_dir.join(".mcp.json"), McpScope::User),
        (context.cwd.join(".mcp.json"), McpScope::Project),
        (context.cwd.join(".claude/.mcp.json"), McpScope::Local),
    ]
}
fn load_mcp_config_file(path: &Path) -> Option<ClaudeCodeMcpConfig> {
    if !path.exists() {
        return None;
    }
    match fs::read_to_string(path)
        .map_err(|e| e.to_string())
        .and_then(|text| serde_json::from_str(&text).map_err(|e| e.to_string()))
    {
        Ok(config) => Some(config),
        Err(error) => {
            log(
                &format!("Failed to load MCP config from {}: {error}", path.display()),
                None,
            );
            None
        }
    }
}
pub fn get_system_mcp_server_names(options: &McpLoaderOptions) -> io::Result<BTreeSet<String>> {
    let context = resolve_mcp_loader_context(options)?;
    let mut names = BTreeSet::new();
    for (path, _) in get_mcp_config_paths(&context) {
        let Some(config) = load_mcp_config_file(&path) else {
            continue;
        };
        for (name, server) in config.mcp_servers {
            if server.disabled == Some(true) {
                names.remove(&name);
                continue;
            }
            if should_load_mcp_server(&server, Some(&context.cwd))? {
                names.insert(name);
            }
        }
    }
    Ok(names)
}
pub fn load_mcp_configs_sync(
    disabled_mcps: &[String],
    options: &McpLoaderOptions,
) -> io::Result<McpLoadResult> {
    let context = resolve_mcp_loader_context(options)?;
    let mut result = McpLoadResult::default();
    for (path, scope) in get_mcp_config_paths(&context) {
        let Some(config) = load_mcp_config_file(&path) else {
            continue;
        };
        for (name, server) in config.mcp_servers {
            if disabled_mcps.contains(&name)
                || !should_load_mcp_server(&server, Some(&context.cwd))?
            {
                continue;
            }
            if server.disabled == Some(true) {
                result.servers.remove(&name);
                result.loaded_servers.retain(|server| server.name != name);
                continue;
            }
            match transform_mcp_server(&name, &server) {
                Ok(config) => {
                    result.servers.insert(name.clone(), config.clone());
                    result.loaded_servers.retain(|server| server.name != name);
                    result.loaded_servers.push(LoadedMcpServer {
                        name,
                        scope,
                        config,
                    });
                }
                Err(error) => log(
                    &format!("Failed to transform MCP server \"{name}\": {error}"),
                    None,
                ),
            }
        }
    }
    Ok(result)
}
pub async fn load_mcp_configs(
    disabled_mcps: &[String],
    options: &McpLoaderOptions,
) -> io::Result<McpLoadResult> {
    load_mcp_configs_sync(disabled_mcps, options)
}
pub fn format_loaded_servers_for_toast(loaded_servers: &[LoadedMcpServer]) -> String {
    loaded_servers
        .iter()
        .map(|server| format!("{} ({})", server.name, server.scope))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn malformed_and_unreadable_configs_do_not_hide_valid_later_configs() {
        let root = tempfile::tempdir().expect("fixture");
        let home = root.path().join("home");
        let cwd = root.path().join("project");
        fs::create_dir_all(home.join(".claude")).expect("home");
        fs::create_dir_all(cwd.join(".claude")).expect("project");
        fs::write(home.join(".claude.json"), "{ invalid").expect("invalid config");
        fs::create_dir(home.join(".claude/.mcp.json")).expect("unreadable config");
        fs::write(
            cwd.join(".mcp.json"),
            json!({"mcpServers":{"same":{"command":"project"},"invalid":{"type":"http"}}})
                .to_string(),
        )
        .expect("project config");
        fs::write(
            cwd.join(".claude/.mcp.json"),
            json!({"mcpServers":{"same":{"command":"local"}}}).to_string(),
        )
        .expect("local config");
        let options = McpLoaderOptions {
            cwd: Some(cwd),
            home_dir: Some(home),
            ..Default::default()
        };
        let result = futures::executor::block_on(load_mcp_configs(&[], &options)).expect("configs");
        assert_eq!(result.servers.len(), 1);
        assert!(
            matches!(&result.servers["same"], McpServerConfig::Local { command, .. } if command == &["local"])
        );
        assert_eq!(result.loaded_servers.len(), 1);
        assert_eq!(result.loaded_servers[0].scope, McpScope::Local);
        assert_eq!(
            format_loaded_servers_for_toast(&result.loaded_servers),
            "same (local)"
        );
        assert_eq!(format_loaded_servers_for_toast(&[]), "");
    }

    #[test]
    fn ambient_context_and_explicit_config_overrides_are_resolved() {
        let context = resolve_mcp_loader_context(&McpLoaderOptions::default()).expect("context");
        assert_eq!(context.cwd, std::env::current_dir().expect("cwd"));
        let home = std::env::var_os("HOME")
            .filter(|v| !v.is_empty())
            .or_else(|| std::env::var_os("USERPROFILE").filter(|v| !v.is_empty()))
            .map(PathBuf::from)
            .or_else(dirs::home_dir)
            .unwrap_or_default();
        assert_eq!(context.home_dir, home);
        assert_eq!(context.claude_config_dir, get_claude_config_dir());
        let context = resolve_mcp_loader_context(&McpLoaderOptions {
            cwd: Some(PathBuf::from("/project")),
            home_dir: Some(PathBuf::from("/home/test")),
            claude_config_dir: Some(PathBuf::from("/custom")),
        })
        .expect("explicit context");
        assert_eq!(context.claude_config_dir, PathBuf::from("/custom"));
    }

    #[test]
    fn config_paths_merge_scopes_filter_local_metadata_and_apply_disables() {
        let root = tempfile::tempdir().expect("fixture directory");
        let home = root.path().join("home");
        let cwd = root.path().join("project");
        for directory in [home.join(".claude"), cwd.join(".claude")] {
            fs::create_dir_all(directory).expect("fixture directory");
        }
        let options = McpLoaderOptions {
            home_dir: Some(home.clone()),
            cwd: Some(cwd.clone()),
            ..Default::default()
        };
        assert!(
            get_system_mcp_server_names(&options)
                .expect("names")
                .is_empty()
        );
        fs::write(
            home.join(".claude.json"),
            json!({"mcpServers":{
                "removed":{"command":"old"}, "global":{"command":"global"},
                "matching":{"command":"matching","scope":"local","projectPath":cwd},
                "other":{"command":"other","scope":"local","projectPath":home},
                "missing":{"command":"missing","scope":"local"}
            }})
            .to_string(),
        )
        .expect("user config");
        fs::write(
            home.join(".claude/.mcp.json"),
            json!({"mcpServers":{"user-config":{"command":"user"}}}).to_string(),
        )
        .expect("user mcp config");
        fs::write(
            cwd.join(".mcp.json"),
            json!({"mcpServers":{"removed":{"disabled":true},"project":{"command":"project"}}})
                .to_string(),
        )
        .expect("project config");
        fs::write(
            cwd.join(".claude/.mcp.json"),
            json!({"mcpServers":{"local":{"command":"local"}}}).to_string(),
        )
        .expect("local config");
        let names = get_system_mcp_server_names(&options).expect("names");
        assert_eq!(
            names,
            ["global", "matching", "user-config", "project", "local"]
                .into_iter()
                .map(str::to_owned)
                .collect::<BTreeSet<_>>()
        );
        let result = futures::executor::block_on(load_mcp_configs(&[], &options)).expect("configs");
        assert_eq!(
            result.servers.keys().cloned().collect::<BTreeSet<_>>(),
            names
        );
        assert_eq!(
            result
                .loaded_servers
                .iter()
                .find(|server| server.name == "matching")
                .expect("matching server")
                .scope,
            McpScope::User
        );
        let disabled = futures::executor::block_on(load_mcp_configs(
            &["global".into(), "project".into()],
            &options,
        ))
        .expect("filtered configs");
        assert!(!disabled.servers.contains_key("global"));
        assert!(!disabled.servers.contains_key("project"));
        assert!(disabled.servers.contains_key("local"));
    }
}
