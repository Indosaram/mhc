use super::{plugin_path_resolver::resolve_plugin_path, types::LoadedPlugin};
use crate::features::claude_code_command_loader::types::{CommandDefinition, CommandRecord};
use crate::shared::logger::log;
use model_core::{CommandSource, sanitize_model_field};
use serde::Deserialize;
use serde_json::Value;
use std::{fs, io, path::Path};
use utils::{
    file_utils::resolve_symlink,
    frontmatter::{FrontmatterMode, parse_frontmatter},
    skill_path_resolver::resolve_skill_path_references,
};

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct SkillMetadata {
    name: Option<String>,
    description: Option<String>,
    model: Option<Value>,
}

pub fn load_plugin_skills_as_commands(plugins: &[LoadedPlugin]) -> io::Result<CommandRecord> {
    let mut skills = CommandRecord::new();

    for plugin in plugins {
        let Some(skills_dir) = plugin.skills_dir.as_ref().filter(|dir| dir.exists()) else {
            continue;
        };

        let entries = match fs::read_dir(skills_dir) {
            Ok(entries) => entries,
            Err(error) => {
                log(
                    &format!(
                        "Failed to read plugin skills directory {}: {error}",
                        skills_dir.display()
                    ),
                    None,
                );
                return Err(error);
            }
        };

        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    log(
                        &format!(
                            "Failed to read entry in skills directory {}: {error}",
                            skills_dir.display()
                        ),
                        None,
                    );
                    continue;
                }
            };

            let entry_name = entry.file_name().to_string_lossy().into_owned();
            if entry_name.starts_with('.') {
                continue;
            }

            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(error) => {
                    log(
                        &format!(
                            "Failed to get file type for {}: {error}",
                            entry.path().display()
                        ),
                        None,
                    );
                    continue;
                }
            };

            if !file_type.is_dir() && !file_type.is_symlink() {
                continue;
            }

            let skill_path = entry.path();
            let resolved_path = resolve_symlink(&skill_path);
            let skill_md_path = resolved_path.join("SKILL.md");
            if !skill_md_path.exists() {
                continue;
            }

            match load_single_skill(
                plugin,
                &entry_name,
                &skill_path,
                &resolved_path,
                &skill_md_path,
            ) {
                Ok((namespaced_name, definition)) => {
                    log(&format!("Loaded plugin skill: {namespaced_name}"), None);
                    skills.insert(namespaced_name, definition);
                }
                Err(error) => {
                    log(
                        &format!(
                            "Failed to load plugin skill: {}: {error}",
                            skill_path.display()
                        ),
                        None,
                    );
                }
            }
        }
    }

    Ok(skills)
}

fn load_single_skill(
    plugin: &LoadedPlugin,
    entry_name: &str,
    _skill_path: &Path,
    resolved_path: &Path,
    skill_md_path: &Path,
) -> io::Result<(String, CommandDefinition)> {
    let content = fs::read_to_string(skill_md_path)?;
    let parsed = parse_frontmatter(&content, FrontmatterMode::Default);
    let data: SkillMetadata = serde_json::from_value(parsed.data).unwrap_or_default();

    let skill_name = data
        .name
        .filter(|n| !n.trim().is_empty())
        .unwrap_or_else(|| entry_name.to_owned());
    let namespaced_name = format!("{}:{}", plugin.name, skill_name);
    let original_description = data.description.unwrap_or_default();
    let formatted_description = format!("(plugin: {} - Skill) {original_description}", plugin.name);

    let resolved_body =
        resolve_skill_path_references(parsed.body.trim(), &resolved_path.to_string_lossy());
    let plugin_resolved_body =
        resolve_plugin_path(&resolved_body, &plugin.install_path.to_string_lossy());

    let base_dir_str = resolved_path.to_string_lossy();
    let base_dir_with_slash = format!("{}/", base_dir_str.trim_end_matches(['/', '\\']));
    let wrapped_template = format!(
        "<skill-instruction>\nBase directory for this skill: {base_dir_with_slash}\nFile references (@path) in this skill are relative to this directory.\n\n{plugin_resolved_body}\n</skill-instruction>\n\n<user-request>\n$ARGUMENTS\n</user-request>"
    );

    let definition = CommandDefinition {
        name: None,
        description: Some(formatted_description),
        template: wrapped_template,
        agent: None,
        model: sanitize_model_field(data.model.as_ref(), CommandSource::ClaudeCode),
        subtask: None,
        argument_hint: None,
        handoffs: None,
    };

    Ok((namespaced_name, definition))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::claude_code_plugin_loader::types::PluginScope;
    use std::fs;

    #[test]
    fn loads_plugin_skills_and_formats_template_and_metadata() {
        let root = tempfile::tempdir().expect("fixture directory");
        let plugin_dir = root.path().join("demo-plugin");
        let skills_dir = plugin_dir.join("skills");
        let my_skill_dir = skills_dir.join("my-skill");
        fs::create_dir_all(&my_skill_dir).expect("skill dir");

        let skill_md_content = "---\nname: custom-name\ndescription: A useful skill\nmodel: opus\n---\nRun ${CLAUDE_PLUGIN_ROOT}/scripts/run.sh on @file.txt";
        fs::write(my_skill_dir.join("SKILL.md"), skill_md_content).expect("SKILL.md write");

        let plugin = LoadedPlugin {
            name: "test-plugin".to_string(),
            version: "1.0.0".to_string(),
            scope: PluginScope::User,
            install_path: plugin_dir.clone(),
            manifest: None,
            plugin_key: "test-plugin@test".to_string(),
            commands_dir: None,
            agents_dir: None,
            skills_dir: Some(skills_dir),
            hooks_path: None,
            mcp_path: None,
            lsp_path: None,
        };

        let skills = load_plugin_skills_as_commands(&[plugin]).expect("load skills");
        assert_eq!(skills.len(), 1);

        let skill = skills
            .get("test-plugin:custom-name")
            .expect("custom-name skill");
        assert_eq!(
            skill.description.as_deref(),
            Some("(plugin: test-plugin - Skill) A useful skill")
        );
        assert!(skill.name.is_none());
        assert!(skill.template.contains("<skill-instruction>"));
        assert!(
            skill
                .template
                .contains("<user-request>\n$ARGUMENTS\n</user-request>")
        );
        assert!(
            skill
                .template
                .contains(&format!("{}/scripts/run.sh", plugin_dir.display()))
        );
        assert!(skill.model.is_none());
    }

    #[test]
    fn falls_back_to_entry_name_when_name_is_missing() {
        let root = tempfile::tempdir().expect("fixture directory");
        let plugin_dir = root.path().join("fallback-plugin");
        let skills_dir = plugin_dir.join("skills");
        let dir_skill = skills_dir.join("dir-skill-name");
        fs::create_dir_all(&dir_skill).expect("skill dir");

        fs::write(dir_skill.join("SKILL.md"), "No frontmatter skill body").expect("write SKILL.md");

        let plugin = LoadedPlugin {
            name: "fallback-plugin".to_string(),
            version: "1.0.0".to_string(),
            scope: PluginScope::User,
            install_path: plugin_dir,
            manifest: None,
            plugin_key: "fallback-plugin@test".to_string(),
            commands_dir: None,
            agents_dir: None,
            skills_dir: Some(skills_dir),
            hooks_path: None,
            mcp_path: None,
            lsp_path: None,
        };

        let skills = load_plugin_skills_as_commands(&[plugin]).expect("load skills");
        assert_eq!(skills.len(), 1);

        let skill = skills
            .get("fallback-plugin:dir-skill-name")
            .expect("fallback skill");
        assert_eq!(
            skill.description.as_deref(),
            Some("(plugin: fallback-plugin - Skill) ")
        );
        assert!(skill.template.contains("No frontmatter skill body"));
    }

    #[test]
    fn ignores_hidden_entries_and_entries_without_skill_md() {
        let root = tempfile::tempdir().expect("fixture directory");
        let plugin_dir = root.path().join("ignore-plugin");
        let skills_dir = plugin_dir.join("skills");
        fs::create_dir_all(skills_dir.join(".hidden-dir")).expect("hidden dir");
        fs::write(skills_dir.join(".hidden-dir").join("SKILL.md"), "hidden").expect("write hidden");

        let no_skill_dir = skills_dir.join("no-skill");
        fs::create_dir_all(&no_skill_dir).expect("no skill dir");
        fs::write(no_skill_dir.join("other.txt"), "other").expect("write other");

        let plugin = LoadedPlugin {
            name: "ignore-plugin".to_string(),
            version: "1.0.0".to_string(),
            scope: PluginScope::User,
            install_path: plugin_dir,
            manifest: None,
            plugin_key: "ignore-plugin@test".to_string(),
            commands_dir: None,
            agents_dir: None,
            skills_dir: Some(skills_dir),
            hooks_path: None,
            mcp_path: None,
            lsp_path: None,
        };

        let skills = load_plugin_skills_as_commands(&[plugin]).expect("load skills");
        assert!(skills.is_empty());
    }
}
