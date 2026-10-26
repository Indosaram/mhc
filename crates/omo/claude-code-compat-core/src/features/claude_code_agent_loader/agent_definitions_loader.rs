use super::{json_agent_loader::parse_json_agent_file, types::*};
use crate::features::claude_code_agent_loader::claude_model_mapper::map_claude_model_to_open_code as map_claude_model_to_provider;
use crate::shared::logger::log;
use crate::shared::parse_tools_config;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};
use utils::frontmatter::{FrontmatterMode, parse_frontmatter};

pub fn parse_markdown_agent_file(
    path: &Path,
    scope: AgentScope,
    anthropic_provider: Option<&str>,
) -> Option<LoadedAgent> {
    let content = match fs::read_to_string(path) {
        Ok(content) => content,
        Err(_) => return None,
    };
    let parsed = parse_frontmatter(&content, FrontmatterMode::Default);
    let data: AgentFrontmatter = match serde_json::from_value(parsed.data) {
        Ok(data) => data,
        Err(_) => return None,
    };
    let filename = path.file_name()?.to_str()?;
    let fallback = if filename.to_ascii_lowercase().ends_with(".md") {
        filename.get(..filename.len().saturating_sub(3))?
    } else {
        filename
    };
    let name = data
        .name
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| fallback.to_owned());
    let model = map_claude_model_to_provider(data.model.as_deref(), anthropic_provider)
        .map(|(provider, model)| AgentModel::String(format!("{provider}/{model}")));
    Some(LoadedAgent {
        name,
        path: path.to_path_buf(),
        scope,
        config: ClaudeCodeAgentConfig {
            description: Some(format!(
                "({scope}) {}",
                data.description.unwrap_or_default()
            )),
            mode: Some(data.mode.unwrap_or_default()),
            prompt: Some(parsed.body.trim().to_owned()),
            model,
            tools: parse_tools_config(data.tools.as_ref()),
        },
    })
}

pub fn load_agent_definitions(
    paths: &[PathBuf],
    scope: AgentScope,
    anthropic_provider: Option<&str>,
) -> BTreeMap<String, ClaudeCodeAgentConfig> {
    let mut result = BTreeMap::new();
    for path in paths {
        if !path.exists() {
            log(
                &format!(
                    "[agent-definitions-loader] File not found, skipping: {}",
                    path.display()
                ),
                None,
            );
            continue;
        }
        let ext = path
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_lowercase();
        let agent = match ext.as_str() {
            "md" => parse_markdown_agent_file(path, scope, anthropic_provider),
            "json" | "jsonc" => parse_json_agent_file(path, scope),
            _ => {
                let ext = if ext.is_empty() {
                    ext
                } else {
                    format!(".{ext}")
                };
                log(
                    &format!(
                        "[agent-definitions-loader] Unsupported file extension: {ext} for {}",
                        path.display()
                    ),
                    None,
                );
                continue;
            }
        };
        match agent {
            Some(agent) => {
                result.insert(agent.name, agent.config);
            }
            None => log(
                &format!(
                    "[agent-definitions-loader] Failed to parse agent file: {}",
                    path.display()
                ),
                None,
            ),
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_markdown_fields_and_provider_override() {
        let dir = tempfile::tempdir().expect("fixture directory");
        let path = dir.path().join("agent.md");
        fs::write(&path, "---\nname: custom\ndescription: A test agent\nmodel: opus\nmode: primary\ntools: Bash, Read\n---\n  Prompt. \n").expect("fixture write");
        let agent = parse_markdown_agent_file(&path, AgentScope::DefinitionFile, Some("kiro"))
            .expect("agent");
        assert_eq!(agent.name, "custom");
        assert_eq!(
            agent.config.description.as_deref(),
            Some("(definition-file) A test agent")
        );
        assert_eq!(agent.config.mode, Some(AgentMode::Primary));
        assert_eq!(agent.config.prompt.as_deref(), Some("Prompt."));
        assert_eq!(
            agent.config.model,
            Some(AgentModel::String("kiro/claude-opus-5-5".into()))
        );
        assert_eq!(
            agent.config.tools,
            Some(BTreeMap::from([
                ("bash".into(), true),
                ("read".into(), true)
            ]))
        );
    }

    #[test]
    fn markdown_defaults_survive_absent_or_malformed_frontmatter() {
        let dir = tempfile::tempdir().expect("fixture directory");
        for (filename, content) in [
            ("UpperCase.MD", "Prompt."),
            (
                "malformed.md",
                "---\ninvalid: yaml: content: here\n---\nPrompt.",
            ),
        ] {
            let path = dir.path().join(filename);
            fs::write(&path, content).expect("fixture write");
            let agent = parse_markdown_agent_file(&path, AgentScope::User, None).expect("agent");
            assert_eq!(
                agent.name,
                path.file_stem().expect("stem").to_string_lossy()
            );
            assert_eq!(agent.config.mode, Some(AgentMode::Subagent));
            assert_eq!(agent.config.prompt.as_deref(), Some("Prompt."));
            assert_eq!(agent.config.description.as_deref(), Some("(user) "));
            assert!(agent.config.tools.is_none());
        }
        assert!(
            parse_markdown_agent_file(&dir.path().join("missing.md"), AgentScope::User, None)
                .is_none()
        );
    }

    #[test]
    fn mixed_definitions_skip_failures_and_last_duplicate_wins() {
        let dir = tempfile::tempdir().expect("fixture directory");
        let fixtures = [
            ("first.md", "---\nname: duplicate\n---\nFirst."),
            (
                "second.md",
                "---\nname: duplicate\nmodel: sonnet\n---\nSecond.",
            ),
            (
                "other.jsonc",
                "{ // comment\n\"name\":\"json-agent\",\"prompt\":\"JSON prompt.\"}",
            ),
            ("broken.json", "{ invalid json"),
            ("unsupported.txt", "Text."),
        ];
        let mut paths = Vec::new();
        for (name, content) in fixtures {
            let path = dir.path().join(name);
            fs::write(&path, content).expect("fixture write");
            paths.push(path);
        }
        paths.push(dir.path().join("missing.md"));
        let agents = load_agent_definitions(&paths, AgentScope::DefinitionFile, Some("kiro"));
        assert_eq!(agents.len(), 2);
        assert_eq!(agents["duplicate"].prompt.as_deref(), Some("Second."));
        assert_eq!(
            agents["duplicate"].model,
            Some(AgentModel::String("kiro/claude-sonnet-5-5".into()))
        );
        assert_eq!(agents["json-agent"].prompt.as_deref(), Some("JSON prompt."));
        assert!(load_agent_definitions(&[], AgentScope::DefinitionFile, None).is_empty());
    }
}
