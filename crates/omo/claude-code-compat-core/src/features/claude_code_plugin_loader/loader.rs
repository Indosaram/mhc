use super::{
    mcp_server_loader::load_plugin_mcp_servers_sync,
    types::{HooksConfig, LoadedPlugin, PluginLoadError, PluginLoadResult, PluginLoaderOptions},
};
use crate::features::{
    claude_code_agent_loader::types::ClaudeCodeAgentConfig,
    claude_code_command_loader::types::CommandRecord,
    claude_code_mcp_loader::types::McpServerConfig,
};
use crate::shared::logger::log;
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, io, sync::Mutex};

pub use super::{
    agent_loader::load_plugin_agents, command_loader::load_plugin_commands,
    discovery::discover_installed_plugins, hook_loader::load_plugin_hooks_configs,
    mcp_server_loader::load_plugin_mcp_servers, skill_loader::load_plugin_skills_as_commands,
};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct PluginComponentsResult {
    pub commands: CommandRecord,
    pub skills: CommandRecord,
    pub agents: BTreeMap<String, ClaudeCodeAgentConfig>,
    pub mcp_servers: BTreeMap<String, McpServerConfig>,
    pub hooks_configs: Vec<HooksConfig>,
    pub plugins: Vec<LoadedPlugin>,
    pub errors: Vec<PluginLoadError>,
}

pub type PluginDiscovery =
    Box<dyn Fn(&PluginLoaderOptions) -> io::Result<PluginLoadResult> + Send + Sync>;
pub type PluginComponentLoader<T> = Box<dyn Fn(&[LoadedPlugin]) -> io::Result<T> + Send + Sync>;
pub type PluginAgentLoader = Box<
    dyn Fn(&[LoadedPlugin], Option<&str>) -> io::Result<BTreeMap<String, ClaudeCodeAgentConfig>>
        + Send
        + Sync,
>;
pub type PluginHooksLoader = Box<dyn Fn(&[LoadedPlugin]) -> Vec<HooksConfig> + Send + Sync>;

pub struct PluginComponentLoadDeps {
    pub discover_installed_plugins: PluginDiscovery,
    pub load_plugin_commands: PluginComponentLoader<CommandRecord>,
    pub load_plugin_skills_as_commands: PluginComponentLoader<CommandRecord>,
    pub load_plugin_agents: PluginAgentLoader,
    pub load_plugin_mcp_servers: PluginComponentLoader<BTreeMap<String, McpServerConfig>>,
    pub load_plugin_hooks_configs: PluginHooksLoader,
}

impl Default for PluginComponentLoadDeps {
    fn default() -> Self {
        Self {
            discover_installed_plugins: Box::new(discover_installed_plugins),
            load_plugin_commands: Box::new(load_plugin_commands),
            load_plugin_skills_as_commands: Box::new(load_plugin_skills_as_commands),
            load_plugin_agents: Box::new(load_plugin_agents),
            load_plugin_mcp_servers: Box::new(load_plugin_mcp_servers_sync),
            load_plugin_hooks_configs: Box::new(load_plugin_hooks_configs),
        }
    }
}

static CACHED_PLUGIN_COMPONENTS: Mutex<BTreeMap<String, PluginComponentsResult>> =
    Mutex::new(BTreeMap::new());

pub fn clear_plugin_components_cache() {
    CACHED_PLUGIN_COMPONENTS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clear();
}

pub fn is_claude_code_plugins_disabled() -> bool {
    fn is_truthy(key: &str) -> bool {
        #[cfg(test)]
        let value = crate::test_support::env_var(key);
        #[cfg(not(test))]
        let value = std::env::var(key);
        match value {
            Ok(val) => val == "true" || val == "1",
            Err(_) => false,
        }
    }
    is_truthy("OPENCODE_DISABLE_CLAUDE_CODE") || is_truthy("OPENCODE_DISABLE_CLAUDE_CODE_PLUGINS")
}

pub fn get_plugin_components_cache_key(options: Option<&PluginLoaderOptions>) -> String {
    let override_entries: Vec<(&String, &bool)> = options
        .and_then(|o| o.enabled_plugins_override.as_ref())
        .map(|m| m.iter().collect())
        .unwrap_or_default();
    let anthropic_provider = options.and_then(|o| o.anthropic_provider.as_deref());

    serde_json::json!({
        "cwd": options.and_then(|o| o.cwd.as_ref()),
        "claudeConfigDir": options.and_then(|o| o.claude_config_dir.as_ref()),
        "pluginsHome": options.and_then(|o| o.plugins_home_override.as_ref()),
        "enabledPluginsOverride": override_entries,
        "anthropicProvider": anthropic_provider,
    })
    .to_string()
}

pub fn load_all_plugin_components_with_deps_sync(
    options: Option<&PluginLoaderOptions>,
    deps: &PluginComponentLoadDeps,
) -> io::Result<PluginComponentsResult> {
    if is_claude_code_plugins_disabled() {
        log(
            "Claude Code plugin loading disabled via OPENCODE_DISABLE_CLAUDE_CODE env var",
            None,
        );
        return Ok(PluginComponentsResult::default());
    }

    let cache_key = get_plugin_components_cache_key(options);
    {
        let cache = CACHED_PLUGIN_COMPONENTS
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(cached) = cache.get(&cache_key) {
            return Ok(cached.clone());
        }
    }

    let default_options = PluginLoaderOptions::default();
    let opts = options.unwrap_or(&default_options);
    let PluginLoadResult { plugins, errors } = (deps.discover_installed_plugins)(opts)?;

    let commands = (deps.load_plugin_commands)(&plugins)?;
    let skills = (deps.load_plugin_skills_as_commands)(&plugins)?;
    let agents = (deps.load_plugin_agents)(&plugins, opts.anthropic_provider.as_deref())?;
    let mcp_servers = (deps.load_plugin_mcp_servers)(&plugins)?;
    let hooks_configs = (deps.load_plugin_hooks_configs)(&plugins);

    log(
        &format!(
            "Loaded {} plugins with {} commands, {} skills, {} agents, {} MCP servers",
            plugins.len(),
            commands.len(),
            skills.len(),
            agents.len(),
            mcp_servers.len(),
        ),
        None,
    );

    let result = PluginComponentsResult {
        commands,
        skills,
        agents,
        mcp_servers,
        hooks_configs,
        plugins,
        errors,
    };

    {
        let mut cache = CACHED_PLUGIN_COMPONENTS
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        cache.insert(cache_key, result.clone());
    }

    Ok(result)
}

pub async fn load_all_plugin_components_with_deps(
    options: Option<&PluginLoaderOptions>,
    deps: &PluginComponentLoadDeps,
) -> io::Result<PluginComponentsResult> {
    load_all_plugin_components_with_deps_sync(options, deps)
}

pub fn load_all_plugin_components_sync(
    options: Option<&PluginLoaderOptions>,
) -> io::Result<PluginComponentsResult> {
    let deps = PluginComponentLoadDeps::default();
    load_all_plugin_components_with_deps_sync(options, &deps)
}

pub async fn load_all_plugin_components(
    options: Option<&PluginLoaderOptions>,
) -> io::Result<PluginComponentsResult> {
    load_all_plugin_components_sync(options)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::{
        claude_code_agent_loader::types::{AgentMode, ClaudeCodeAgentConfig},
        claude_code_command_loader::types::CommandDefinition,
        claude_code_mcp_loader::types::McpServerConfig,
        claude_code_plugin_loader::types::PluginScope,
    };
    use std::{
        path::PathBuf,
        sync::atomic::{AtomicUsize, Ordering},
    };

    #[test]
    fn native_roots_and_session_directories_have_distinct_cache_keys() {
        // Given
        let root = PluginLoaderOptions { cwd: Some(PathBuf::from("/project/a")),
            plugins_home_override: Some(PathBuf::from("/home/a/plugins")), ..Default::default() };
        let mut other_project = root.clone();
        other_project.cwd = Some(PathBuf::from("/project/b"));
        let mut other_home = root.clone();
        other_home.plugins_home_override = Some(PathBuf::from("/home/b/plugins"));
        // When
        let key = get_plugin_components_cache_key(Some(&root));
        // Then
        assert_ne!(key, get_plugin_components_cache_key(Some(&other_project)));
        assert_ne!(key, get_plugin_components_cache_key(Some(&other_home)));
    }

    #[test]
    fn enabled_override_order_does_not_change_cache_key() {
        let first = PluginLoaderOptions {
            enabled_plugins_override: Some(BTreeMap::from([
                ("b".into(), false),
                ("a".into(), true),
            ])),
            ..Default::default()
        };
        let second = PluginLoaderOptions {
            enabled_plugins_override: Some(BTreeMap::from([
                ("a".into(), true),
                ("b".into(), false),
            ])),
            ..Default::default()
        };
        assert_eq!(
            get_plugin_components_cache_key(Some(&first)),
            get_plugin_components_cache_key(Some(&second))
        );
        assert_ne!(
            get_plugin_components_cache_key(Some(&first)),
            get_plugin_components_cache_key(None)
        );
    }

    fn create_test_components_result() -> PluginComponentsResult {
        let mut commands = CommandRecord::new();
        commands.insert(
            "demo:command".to_string(),
            CommandDefinition {
                name: Some("demo:command".to_string()),
                description: Some("demo".to_string()),
                template: "demo".to_string(),
                agent: None,
                model: None,
                subtask: None,
                argument_hint: None,
                handoffs: None,
            },
        );

        let mut skills = CommandRecord::new();
        skills.insert(
            "demo:skill".to_string(),
            CommandDefinition {
                name: Some("demo:skill".to_string()),
                description: Some("skill".to_string()),
                template: "skill".to_string(),
                agent: None,
                model: None,
                subtask: None,
                argument_hint: None,
                handoffs: None,
            },
        );

        let mut agents = BTreeMap::new();
        agents.insert(
            "demo:agent".to_string(),
            ClaudeCodeAgentConfig {
                description: Some("agent".to_string()),
                mode: Some(AgentMode::Subagent),
                prompt: Some("demo".to_string()),
                model: None,
                tools: None,
            },
        );

        let mut mcp_servers = BTreeMap::new();
        mcp_servers.insert(
            "demo:mcp".to_string(),
            McpServerConfig::Local {
                command: vec!["demo".to_string()],
                environment: None,
                enabled: true,
            },
        );

        let plugins = vec![LoadedPlugin {
            name: "demo".to_string(),
            version: "1.0.0".to_string(),
            scope: PluginScope::User,
            install_path: PathBuf::from("/tmp/demo"),
            manifest: None,
            plugin_key: "demo@test".to_string(),
            commands_dir: None,
            agents_dir: None,
            skills_dir: None,
            hooks_path: None,
            mcp_path: None,
            lsp_path: None,
        }];

        PluginComponentsResult {
            commands,
            skills,
            agents,
            mcp_servers,
            hooks_configs: vec![serde_json::json!({"hooks": {}})],
            plugins,
            errors: Vec::new(),
        }
    }

    #[test]
    fn disabled_via_env_var_returns_empty_result() {
        let _guard = crate::TEST_PROCESS_STATE
            .lock()
            .expect("process state lock");
        for (var, val) in [
            ("OPENCODE_DISABLE_CLAUDE_CODE", "true"),
            ("OPENCODE_DISABLE_CLAUDE_CODE", "1"),
            ("OPENCODE_DISABLE_CLAUDE_CODE_PLUGINS", "true"),
            ("OPENCODE_DISABLE_CLAUDE_CODE_PLUGINS", "1"),
        ] {
            let _env = crate::test_support::EnvOverride::set(var, val);
            assert!(is_claude_code_plugins_disabled());
            let result = load_all_plugin_components_sync(None).expect("load components");
            assert!(result.commands.is_empty());
            assert!(result.skills.is_empty());
            assert!(result.agents.is_empty());
            assert!(result.mcp_servers.is_empty());
            assert!(result.hooks_configs.is_empty());
            assert!(result.plugins.is_empty());
            assert!(result.errors.is_empty());
        }
    }

    #[test]
    fn unrecognized_env_var_value_does_not_disable_loading() {
        let _guard = crate::TEST_PROCESS_STATE
            .lock()
            .expect("process state lock");
        let _env = crate::test_support::EnvOverride::set("OPENCODE_DISABLE_CLAUDE_CODE", "yes");
        assert!(!is_claude_code_plugins_disabled());
    }

    #[test]
    fn repeated_calls_with_same_options_use_cached_result() {
        let _guard = crate::TEST_PROCESS_STATE
            .lock()
            .expect("process state lock");
        clear_plugin_components_cache();
        let expected = create_test_components_result();

        let discover_count = std::sync::Arc::new(AtomicUsize::new(0));
        let counter = discover_count.clone();
        let counters = std::sync::Arc::new([const { AtomicUsize::new(0) }; 5]);
        let commands_count = counters.clone();
        let skills_count = counters.clone();
        let agents_count = counters.clone();
        let mcp_count = counters.clone();
        let hooks_count = counters.clone();

        let exp_plugins = expected.plugins.clone();
        let exp_errors = expected.errors.clone();
        let exp_commands = expected.commands.clone();
        let exp_skills = expected.skills.clone();
        let exp_agents = expected.agents.clone();
        let exp_mcp = expected.mcp_servers.clone();
        let exp_hooks = expected.hooks_configs.clone();

        let deps = PluginComponentLoadDeps {
            discover_installed_plugins: Box::new(move |_| {
                counter.fetch_add(1, Ordering::SeqCst);
                Ok(PluginLoadResult {
                    plugins: exp_plugins.clone(),
                    errors: exp_errors.clone(),
                })
            }),
            load_plugin_commands: Box::new(move |_| {
                commands_count[0].fetch_add(1, Ordering::SeqCst);
                Ok(exp_commands.clone())
            }),
            load_plugin_skills_as_commands: Box::new(move |_| {
                skills_count[1].fetch_add(1, Ordering::SeqCst);
                Ok(exp_skills.clone())
            }),
            load_plugin_agents: Box::new(move |_, _| {
                agents_count[2].fetch_add(1, Ordering::SeqCst);
                Ok(exp_agents.clone())
            }),
            load_plugin_mcp_servers: Box::new(move |_| {
                mcp_count[3].fetch_add(1, Ordering::SeqCst);
                Ok(exp_mcp.clone())
            }),
            load_plugin_hooks_configs: Box::new(move |_| {
                hooks_count[4].fetch_add(1, Ordering::SeqCst);
                exp_hooks.clone()
            }),
        };

        let mut overrides = BTreeMap::new();
        overrides.insert("demo@test".to_string(), true);
        let options = PluginLoaderOptions {
            enabled_plugins_override: Some(overrides),
            ..Default::default()
        };

        let first =
            load_all_plugin_components_with_deps_sync(Some(&options), &deps).expect("first load");
        let second =
            load_all_plugin_components_with_deps_sync(Some(&options), &deps).expect("second load");

        assert_eq!(first, expected);
        assert_eq!(second, expected);
        assert_eq!(discover_count.load(Ordering::SeqCst), 1);
        let mut mutated = first;
        mutated.commands.clear();
        let third =
            load_all_plugin_components_with_deps_sync(Some(&options), &deps).expect("cached clone");
        assert_eq!(third, expected);
        for count in counters.iter() {
            assert_eq!(count.load(Ordering::SeqCst), 1);
        }
    }

    #[test]
    fn cache_invalidation_when_options_change_or_cache_cleared() {
        let _guard = crate::TEST_PROCESS_STATE
            .lock()
            .expect("process state lock");
        clear_plugin_components_cache();
        let expected = create_test_components_result();

        let calls = std::sync::Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        let providers = std::sync::Arc::new(Mutex::new(Vec::new()));
        let captured_providers = providers.clone();
        let exp_plugins = expected.plugins.clone();
        let exp_errors = expected.errors.clone();
        let exp_commands = expected.commands.clone();
        let exp_skills = expected.skills.clone();
        let exp_agents = expected.agents.clone();
        let exp_mcp = expected.mcp_servers.clone();
        let exp_hooks = expected.hooks_configs.clone();

        let deps = PluginComponentLoadDeps {
            discover_installed_plugins: Box::new(move |_| {
                counter.fetch_add(1, Ordering::SeqCst);
                Ok(PluginLoadResult {
                    plugins: exp_plugins.clone(),
                    errors: exp_errors.clone(),
                })
            }),
            load_plugin_commands: Box::new(move |_| Ok(exp_commands.clone())),
            load_plugin_skills_as_commands: Box::new(move |_| Ok(exp_skills.clone())),
            load_plugin_agents: Box::new(move |_, provider| {
                captured_providers
                    .lock()
                    .expect("providers")
                    .push(provider.map(str::to_owned));
                Ok(exp_agents.clone())
            }),
            load_plugin_mcp_servers: Box::new(move |_| Ok(exp_mcp.clone())),
            load_plugin_hooks_configs: Box::new(move |_| exp_hooks.clone()),
        };

        let opt1 = PluginLoaderOptions {
            anthropic_provider: Some("provider-a".to_string()),
            ..Default::default()
        };
        let opt2 = PluginLoaderOptions {
            anthropic_provider: Some("provider-b".to_string()),
            ..Default::default()
        };

        load_all_plugin_components_with_deps_sync(Some(&opt1), &deps).expect("load 1");
        load_all_plugin_components_with_deps_sync(Some(&opt2), &deps).expect("load 2");

        clear_plugin_components_cache();
        load_all_plugin_components_with_deps_sync(Some(&opt1), &deps).expect("load 3 after clear");
        assert_eq!(calls.load(Ordering::SeqCst), 3);
        let enabled = PluginLoaderOptions {
            enabled_plugins_override: Some(BTreeMap::from([("demo@test".into(), true)])),
            ..Default::default()
        };
        let disabled = PluginLoaderOptions {
            enabled_plugins_override: Some(BTreeMap::from([("demo@test".into(), false)])),
            ..Default::default()
        };
        load_all_plugin_components_with_deps_sync(Some(&enabled), &deps).expect("enabled override");
        load_all_plugin_components_with_deps_sync(Some(&disabled), &deps)
            .expect("disabled override");
        assert_eq!(calls.load(Ordering::SeqCst), 5);
        assert_eq!(
            *providers.lock().expect("providers"),
            vec![
                Some("provider-a".into()),
                Some("provider-b".into()),
                Some("provider-a".into()),
                None,
                None
            ]
        );
    }
}
