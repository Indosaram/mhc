use super::{plugin_path_resolver::resolve_plugin_path, types::LoadedPlugin};
use crate::features::claude_code_agent_loader::claude_model_mapper::map_claude_model_to_open_code as map_claude_model_to_provider;
use crate::shared::logger::log;
use crate::{
    features::claude_code_agent_loader::{
        AgentFrontmatter, AgentMode, AgentModel, ClaudeCodeAgentConfig,
    },
    shared::parse_tools_config,
};
use std::{collections::BTreeMap, fs, io};
use utils::{
    file_utils::is_markdown_file,
    frontmatter::{FrontmatterMode, parse_frontmatter},
};

pub fn load_plugin_agents(
    plugins: &[LoadedPlugin],
    anthropic_provider: Option<&str>,
) -> io::Result<BTreeMap<String, ClaudeCodeAgentConfig>> {
    let mut agents = BTreeMap::new();
    for plugin in plugins {
        let Some(directory) = plugin
            .agents_dir
            .as_ref()
            .filter(|directory| directory.exists())
        else {
            continue;
        };
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            let filename = entry.file_name().to_string_lossy().into_owned();
            if !is_markdown_file(&filename, entry.file_type()?.is_file()) {
                continue;
            }
            let name = format!(
                "{}:{}",
                plugin.name,
                filename.strip_suffix(".md").unwrap_or(&filename)
            );
            let path = entry.path();
            let content = match fs::read_to_string(&path) {
                Ok(content) => content,
                Err(error) => {
                    log(
                        &format!("Failed to load plugin agent: {}: {error}", path.display()),
                        None,
                    );
                    continue;
                }
            };
            let parsed = parse_frontmatter(&content, FrontmatterMode::Default);
            let data: AgentFrontmatter = match serde_json::from_value(parsed.data) {
                Ok(data) => data,
                Err(error) => {
                    log(
                        &format!("Failed to load plugin agent: {}: {error}", path.display()),
                        None,
                    );
                    continue;
                }
            };
            let model = map_claude_model_to_provider(data.model.as_deref(), anthropic_provider)
                .map(|(provider, model)| AgentModel::String(format!("{provider}/{model}")));
            agents.insert(
                name,
                ClaudeCodeAgentConfig {
                    description: Some(format!(
                        "(plugin: {}) {}",
                        plugin.name,
                        data.description.unwrap_or_default()
                    )),
                    mode: Some(AgentMode::Subagent),
                    prompt: Some(resolve_plugin_path(
                        parsed.body.trim(),
                        &plugin.install_path.to_string_lossy(),
                    )),
                    model,
                    tools: parse_tools_config(data.tools.as_ref()),
                },
            );
        }
    }
    Ok(agents)
}
