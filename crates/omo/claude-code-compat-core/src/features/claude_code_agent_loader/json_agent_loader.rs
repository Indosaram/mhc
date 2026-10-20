use super::types::*;
use crate::features::claude_code_agent_loader::claude_model_mapper::map_claude_model_to_open_code as map_claude_model_to_provider;
use crate::shared::parse_tools_config;
use std::{fs, path::Path};
use utils::jsonc_parser::parse_jsonc_safe;

pub fn parse_json_agent_file(path: &Path, scope: AgentScope) -> Option<LoadedAgent> {
    let content = match fs::read_to_string(path) {
        Ok(content) => content,
        Err(_) => return None,
    };
    let value = parse_jsonc_safe(&content).data?;
    let data: AgentJsonDefinition = match serde_json::from_value(value) {
        Ok(data) => data,
        Err(_) => return None,
    };
    if data.name.is_empty() || data.prompt.is_empty() {
        return None;
    }
    let model = map_claude_model_to_provider(data.model.as_deref(), None)
        .map(|(provider, model)| AgentModel::String(format!("{provider}/{model}")));
    Some(LoadedAgent {
        name: data.name,
        path: path.to_path_buf(),
        scope,
        config: ClaudeCodeAgentConfig {
            description: Some(format!(
                "({scope}) {}",
                data.description.unwrap_or_default()
            )),
            mode: Some(data.mode.unwrap_or_default()),
            prompt: Some(data.prompt.trim().to_owned()),
            model,
            tools: parse_tools_config(data.tools.as_ref()),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_json_fields_and_both_tools_formats() {
        let dir = tempfile::tempdir().expect("fixture directory");
        for tools in [json!(["Bash", " Read "]), json!("Bash, Read")] {
            let path = dir.path().join("agent.json");
            fs::write(&path, json!({"name":"agent", "prompt":" Prompt. ", "description":"Agent", "tools":tools, "model":"claude-3-5-sonnet-20241022"}).to_string()).expect("fixture write");
            let agent = parse_json_agent_file(&path, AgentScope::DefinitionFile).expect("agent");
            assert_eq!(agent.name, "agent");
            assert_eq!(agent.path, path);
            assert_eq!(agent.scope, AgentScope::DefinitionFile);
            assert_eq!(
                agent.config.description.as_deref(),
                Some("(definition-file) Agent")
            );
            assert_eq!(agent.config.prompt.as_deref(), Some("Prompt."));
            assert_eq!(agent.config.mode, Some(AgentMode::Subagent));
            assert_eq!(
                agent.config.tools,
                Some(std::collections::BTreeMap::from([
                    ("bash".into(), true),
                    ("read".into(), true)
                ]))
            );
        }
    }

    #[test]
    fn accepts_jsonc_and_defaults_optional_fields() {
        let dir = tempfile::tempdir().expect("fixture directory");
        let path = dir.path().join("agent.jsonc");
        fs::write(
            &path,
            "{ // name\n\"name\":\"minimal\",\"prompt\":\"Prompt.\",}",
        )
        .expect("fixture write");
        let agent = parse_json_agent_file(&path, AgentScope::User).expect("agent");
        assert_eq!(agent.config.description.as_deref(), Some("(user) "));
        assert_eq!(agent.config.mode, Some(AgentMode::Subagent));
        assert!(agent.config.model.is_none());
        assert!(agent.config.tools.is_none());
    }

    #[test]
    fn rejects_missing_empty_and_malformed_required_fields() {
        let dir = tempfile::tempdir().expect("fixture directory");
        let path = dir.path().join("agent.json");
        for content in [
            "{\"prompt\":\"P\"}",
            "{\"name\":\"N\"}",
            "{\"name\":\"\",\"prompt\":\"P\"}",
            "{\"name\":\"N\",\"prompt\":\"\"}",
            "{ invalid",
        ] {
            fs::write(&path, content).expect("fixture write");
            assert!(parse_json_agent_file(&path, AgentScope::User).is_none());
        }
        assert!(
            parse_json_agent_file(&dir.path().join("missing.json"), AgentScope::User).is_none()
        );
    }
}
