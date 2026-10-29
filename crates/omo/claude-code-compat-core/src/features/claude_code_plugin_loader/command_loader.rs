use super::{plugin_path_resolver::resolve_plugin_path, types::LoadedPlugin};
use crate::features::claude_code_command_loader::{
    CommandDefinition, CommandFrontmatter, CommandRecord,
};
use crate::shared::logger::log;
use model_core::{CommandSource, sanitize_model_field};
use std::{fs, io};
use utils::{
    file_utils::is_markdown_file,
    frontmatter::{FrontmatterMode, parse_frontmatter},
};

pub fn load_plugin_commands(plugins: &[LoadedPlugin]) -> io::Result<CommandRecord> {
    let mut commands = CommandRecord::new();
    for plugin in plugins {
        let Some(directory) = plugin
            .commands_dir
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
                        &format!("Failed to load plugin command: {}: {error}", path.display()),
                        None,
                    );
                    continue;
                }
            };
            let parsed = parse_frontmatter(&content, FrontmatterMode::Default);
            let data: CommandFrontmatter = match serde_json::from_value(parsed.data) {
                Ok(data) => data,
                Err(error) => {
                    log(
                        &format!("Failed to load plugin command: {}: {error}", path.display()),
                        None,
                    );
                    continue;
                }
            };
            let body = resolve_plugin_path(&parsed.body, &plugin.install_path.to_string_lossy());
            commands.insert(name, CommandDefinition {
                name: None, description: Some(format!("(plugin: {}) {}", plugin.name, data.description.unwrap_or_default())),
                template: format!("<command-instruction>\n{}\n</command-instruction>\n\n<user-request>\n$ARGUMENTS\n</user-request>", body.trim()),
                agent: data.agent, model: sanitize_model_field(data.model.as_ref(), CommandSource::ClaudeCode),
                subtask: data.subtask, argument_hint: None, handoffs: None });
        }
    }
    Ok(commands)
}
