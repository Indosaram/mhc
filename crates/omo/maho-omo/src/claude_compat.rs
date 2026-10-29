//! Claude Code resources consumed by the native extension host, without OpenCode discovery.

use std::{collections::{BTreeMap, HashSet}, path::{Path, PathBuf}, sync::Arc};
use claude_code_compat_core as compat;
use maho_ext_api::{ExtensionApi, ExtensionFailure, McpServerDeclaration, McpTransport,
    EventKind, EventResult, ResourcesDiscoverResult, SendUserMessageOptions, UserMessageContent};

pub struct NativeClaudeResources {
    pub commands: compat::CommandRecord,
    pub mcp_servers: BTreeMap<String, compat::McpServerConfig>,
    pub skill_paths: Vec<PathBuf>,
}

pub fn plugin_options(cwd: &Path, claude_dir: &Path) -> compat::PluginLoaderOptions {
    compat::PluginLoaderOptions {
        cwd: Some(cwd.to_owned()),
        claude_config_dir: Some(claude_dir.to_owned()),
        plugins_home_override: Some(claude_dir.join("plugins")),
        ..Default::default()
    }
}

impl NativeClaudeResources {
    pub fn load(cwd: &Path, home: &Path, claude_dir: &Path) -> std::io::Result<Self> {
        let plugins = compat::load_all_plugin_components_sync(Some(&plugin_options(cwd, claude_dir)))?;
        for error in &plugins.errors {
            maho_ext_api::diagnostic(&format!("Claude plugin {}: {}", error.plugin_key, error.error));
        }
        let mut commands = plugins.skills;
        commands.extend(plugins.commands);
        // Later scopes override earlier ones; retain argument hints for native completion.
        for (directory, scope) in [(claude_dir.join("commands"), compat::CommandScope::User),
            (cwd.join(".claude/commands"), compat::CommandScope::Project)] {
            for command in compat::load_commands_from_dir(&directory, scope, &mut HashSet::new(), "") {
                commands.insert(command.name, command.definition);
            }
        }
        let mut mcp_servers = plugins.mcp_servers;
        mcp_servers.extend(compat::load_mcp_configs_sync(&[], &compat::McpLoaderOptions {
            cwd: Some(cwd.to_owned()), home_dir: Some(home.to_owned()),
            claude_config_dir: Some(claude_dir.to_owned()),
        })?.servers);
        let mut skill_paths = vec![claude_dir.join("skills"), cwd.join(".claude/skills")];
        skill_paths.extend(plugins.plugins.into_iter().filter_map(|plugin| plugin.skills_dir));
        skill_paths.retain(|path| path.is_dir());
        Ok(Self { commands, mcp_servers, skill_paths })
    }

    pub fn register(self, api: &mut ExtensionApi) {
        let sender = Arc::new(ExtensionApi::new(api.registered.clone(), api.profile.clone(),
            api.events.clone(), api.runtime.clone()));
        for (name, command) in self.commands {
            let sender = sender.clone();
            api.register_command(&name, command.description, command.argument_hint,
                Arc::new(move |arguments, context| {
                    let prompt = command.template.replace("$ARGUMENTS", arguments);
                    let sender = sender.clone();
                    let agent = command.agent.clone();
                    let model = command.model.clone();
                    let subtask = command.subtask == Some(true);
                    Box::pin(async move {
                        if agent.is_some() || subtask {
                            let mut params = serde_json::json!({"prompt":prompt});
                            if let Some(agent) = agent { params["subagent_type"] = agent.into(); }
                            if let Some(model) = model { params["model"] = model.into(); }
                            let controller = maho_ai::utils::abort::AbortController::new();
                            if context.signal.as_ref().is_some_and(|signal| signal.is_aborted()) {
                                controller.abort(None);
                            }
                            let execution = sender.execute_tool("task", params, maho_ext_api::ExecuteToolOptions {
                                signal: context.signal.as_ref().map(|_| controller.signal()),
                                ..Default::default()
                            });
                            tokio::pin!(execution);
                            let result = if let Some(signal) = &context.signal {
                                tokio::select! {
                                    biased;
                                    () = signal.cancelled() => {
                                        controller.abort(None);
                                        execution.await
                                    }
                                    result = &mut execution => result,
                                }
                            } else {
                                execution.await
                            };
                            result.map_err(|error| ExtensionFailure::new(error.to_string()))?;
                            return Ok(());
                        }
                        if let Some(selector) = model {
                            let (provider, id) = selector.split_once('/').ok_or_else(||
                                ExtensionFailure::new(format!("Claude command model requires provider/model: {selector}")))?;
                            let model = context.model_registry.find(provider, id).ok_or_else(||
                                ExtensionFailure::new(format!("Claude command model unavailable: {selector}")))?;
                            if !sender.set_model(model).await? {
                                return Err(ExtensionFailure::new(format!("Claude command model rejected: {selector}")));
                            }
                        }
                        sender.send_user_message(UserMessageContent::Text(prompt), SendUserMessageOptions::default())
                    })
                }));
        }
        for (name, config) in self.mcp_servers {
            if let Err(error) = api.try_register_mcp_server(&name, mcp_declaration(config)) {
                maho_ext_api::diagnostic(&format!("Claude MCP {name}: {error}"));
            }
        }
        let skill_paths = self.skill_paths;
        api.on(EventKind::ResourcesDiscover, Arc::new(move |_, _| {
            let skill_paths = skill_paths.iter().map(|path| path.to_string_lossy().into_owned().into()).collect();
            Box::pin(async move { Ok(EventResult::ResourcesDiscover(ResourcesDiscoverResult {
                skill_paths, ..Default::default()
            })) })
        }));
    }
}

pub fn register(api: &mut ExtensionApi, env: &BTreeMap<String, String>) -> Result<(), ExtensionFailure> {
    compat::clear_plugin_components_cache();
    let claude_dir = env.get("CLAUDE_CONFIG_DIR").filter(|value| !value.is_empty())
        .map(PathBuf::from).unwrap_or_else(compat::shared::get_claude_config_dir);
    let home = env.get("HOME").or_else(|| env.get("USERPROFILE")).map(PathBuf::from)
        .or_else(|| claude_dir.parent().map(Path::to_owned)).unwrap_or_default();
    NativeClaudeResources::load(&api.cwd, &home, &claude_dir)
        .map_err(|error| ExtensionFailure::new(error.to_string()))?.register(api);
    Ok(())
}

fn mcp_declaration(config: compat::McpServerConfig) -> McpServerDeclaration {
    match config {
        compat::McpServerConfig::Local { command, environment, enabled } => {
            let mut command = command.into_iter();
            McpServerDeclaration { transport: Some(McpTransport::Stdio), command: command.next(),
                args: Some(command.collect()), env: environment, enabled: Some(enabled), ..Default::default() }
        }
        compat::McpServerConfig::Remote { url, headers, oauth, enabled } => McpServerDeclaration {
            transport: Some(McpTransport::Http), url: Some(url), headers, enabled: Some(enabled),
            oauth: oauth.map(|oauth| maho_ext_api::McpOAuth { client_id: oauth.client_id,
                scopes: oauth.scopes, ..Default::default() }), ..Default::default()
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_resources_load_project_overrides_without_opencode_discovery() -> std::io::Result<()> {
        // Given
        let root = tempfile::tempdir()?;
        let home = root.path().join("home");
        let claude = home.join(".claude");
        let cwd = root.path().join("project");
        for path in [claude.join("commands"), cwd.join(".claude/commands"),
            cwd.join(".claude/skills/example"), cwd.join(".opencode/commands")] {
            std::fs::create_dir_all(path)?;
        }
        std::fs::write(claude.join("commands/shared.md"), "user")?;
        std::fs::write(cwd.join(".claude/commands/shared.md"), "---\nargument-hint: file\n---\nproject")?;
        std::fs::write(cwd.join(".opencode/commands/ignored.md"), "ignored")?;
        std::fs::write(cwd.join(".mcp.json"), r#"{"mcpServers":{"local":{"command":"native","args":["serve"]}}}"#)?;
        // When
        let resources = NativeClaudeResources::load(&cwd, &home, &claude)?;
        // Then
        assert_eq!(resources.commands.len(), 1);
        assert_eq!(resources.commands["shared"].argument_hint.as_deref(), Some("file"));
        assert_eq!(resources.skill_paths, vec![cwd.join(".claude/skills")]);
        let declaration = mcp_declaration(resources.mcp_servers["local"].clone());
        assert_eq!(declaration.command.as_deref(), Some("native"));
        assert_eq!(declaration.args, Some(vec!["serve".into()]));
        Ok(())
    }

    #[test]
    fn remote_mcp_keeps_native_oauth_and_disabled_state() {
        // Given
        let config = compat::McpServerConfig::Remote { url: "https://example.test/mcp".into(),
            headers: Some(BTreeMap::from([("X-Test".into(), "value".into())])),
            oauth: Some(compat::McpOAuthConfig { client_id: Some("native".into()), scopes: Some(vec!["read".into()]) }),
            enabled: false };
        // When
        let declaration = mcp_declaration(config);
        // Then
        assert_eq!(declaration.transport, Some(McpTransport::Http));
        assert_eq!(declaration.enabled, Some(false));
        let oauth = declaration.oauth.expect("OAuth declaration");
        assert_eq!(oauth.client_id.as_deref(), Some("native"));
        assert_eq!(oauth.scopes, Some(vec!["read".into()]));
    }

    #[test]
    fn loaded_commands_and_mcps_reach_the_native_host_registration() {
        // Given
        let resources = NativeClaudeResources {
            commands: BTreeMap::from([("demo:run".into(), compat::CommandDefinition {
                argument_hint: Some("path".into()), template: "$ARGUMENTS".into(), ..Default::default()
            })]),
            mcp_servers: BTreeMap::from([("demo:mcp".into(), compat::McpServerConfig::Local {
                command: vec!["native".into(), "serve".into()], environment: None, enabled: true
            })]),
            skill_paths: Vec::new(),
        };
        let mut api = ExtensionApi::new(
            maho_ext_api::LoadedExtension::new("compat-test", PathBuf::from("/repo"), Default::default()),
            Default::default(), Default::default(), Default::default());
        // When
        resources.register(&mut api);
        // Then
        assert_eq!(api.registered.commands.len(), 1);
        assert_eq!(api.registered.commands[0].name, "demo:run");
        assert_eq!(api.registered.commands[0].argument_hint.as_deref(), Some("path"));
        assert_eq!(api.registered.mcp_servers.len(), 1);
        assert_eq!(api.registered.mcp_servers[0].config.command.as_deref(), Some("native"));
    }

    #[test]
    fn installed_plugin_commands_skills_and_mcps_enter_native_resources() -> std::io::Result<()> {
        // Given
        let root = tempfile::tempdir()?;
        let home = root.path().join("home");
        let claude = home.join(".claude");
        let cwd = root.path().join("project");
        let plugin = root.path().join("plugin");
        for path in [claude.join("plugins"), cwd.clone(), plugin.join("commands"), plugin.join("skills/check")] {
            std::fs::create_dir_all(path)?;
        }
        std::fs::write(claude.join("plugins/installed_plugins.json"), serde_json::json!({
            "version":1,"plugins":{"demo@test":{"installPath":plugin,"scope":"user","version":"1"}}
        }).to_string())?;
        std::fs::write(plugin.join("commands/run.md"), "Run")?;
        std::fs::write(plugin.join("skills/check/SKILL.md"), "---\nname: check\ndescription: Check\n---\nCheck")?;
        std::fs::write(plugin.join(".mcp.json"), r#"{"mcpServers":{"local":{"command":"native"}}}"#)?;
        // When
        let resources = NativeClaudeResources::load(&cwd, &home, &claude)?;
        // Then
        assert!(resources.commands.contains_key("demo:run"));
        assert!(resources.commands.contains_key("demo:check"));
        assert_eq!(resources.skill_paths, vec![plugin.join("skills")]);
        assert_eq!(resources.mcp_servers.len(), 1);
        Ok(())
    }
}
