use super::{
    agent_definitions_loader::load_agent_definitions,
    claude_model_mapper::map_claude_model_to_open_code, types::*,
};
use crate::shared::{
    jsonc_parser::parse_jsonc_safe,
    opencode_config_dir::{OpenCodeConfigDirOptions, get_open_code_config_dirs},
    parse_tools_config, resolve_agent_definition_paths,
};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

fn convert_inline_agent(value: &Value) -> Option<ClaudeCodeAgentConfig> {
    let agent = value.as_object()?;
    let text = |key: &str| {
        agent
            .get(key)
            .filter(|v| !v.is_null() && **v != Value::Bool(false) && v.as_i64() != Some(0))
            .map(|v| {
                v.as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| v.to_string())
            })
            .unwrap_or_default()
    };
    let model = map_claude_model_to_open_code(Some(&text("model")), None)
        .map(|(provider, model)| AgentModel::String(format!("{provider}/{model}")));
    let mode = match agent.get("mode").and_then(Value::as_str) {
        Some("primary") => AgentMode::Primary,
        Some("all") => AgentMode::All,
        _ => AgentMode::Subagent,
    };
    Some(ClaudeCodeAgentConfig {
        description: Some(format!("(opencode-config) {}", text("description"))),
        mode: Some(mode),
        prompt: Some(text("prompt")),
        model,
        tools: parse_tools_config(agent.get("tools")),
    })
}

pub fn read_opencode_config_agents(directory: &Path) -> BTreeMap<String, ClaudeCodeAgentConfig> {
    let mut roots = vec![directory.join(".opencode")];
    roots.extend(get_open_code_config_dirs(
        &OpenCodeConfigDirOptions::default(),
    ));
    read_config_agents_from_roots(directory, &roots)
}

fn read_config_agents_from_roots(
    directory: &Path,
    roots: &[PathBuf],
) -> BTreeMap<String, ClaudeCodeAgentConfig> {
    let mut result = BTreeMap::new();
    for root in roots {
        for filename in ["opencode.json", "opencode.jsonc"] {
            let Ok(content) = fs::read_to_string(root.join(filename)) else {
                continue;
            };
            let Some(config) = parse_jsonc_safe(&content).data else {
                continue;
            };
            if let Some(agents) = config
                .get("agents")
                .or_else(|| config.get("agent"))
                .and_then(Value::as_object)
            {
                for (name, value) in agents {
                    if result.contains_key(name) {
                        continue;
                    }
                    if let Some(agent) = convert_inline_agent(value) {
                        result.insert(name.clone(), agent);
                    }
                }
            }
            let definitions = match config.get("agent_definitions") {
                Some(Value::String(path)) => vec![path.clone()],
                Some(Value::Array(paths)) => paths
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect(),
                _ => Vec::new(),
            };
            let paths = resolve_agent_definition_paths(&definitions, root, Some(directory));
            for (name, agent) in load_agent_definitions(&paths, AgentScope::OpencodeConfig, None) {
                result.entry(name).or_insert(agent);
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn public_reader_merges_custom_and_default_user_configs_after_project() {
        let root = tempfile::tempdir().expect("fixture");
        let project = root.path().join("project");
        let custom = root.path().join("custom");
        let xdg = root.path().join("xdg");
        let default = xdg.join("opencode");
        for directory in [project.join(".opencode"), custom.clone(), default.clone()] {
            fs::create_dir_all(directory).expect("directory");
        }
        for (directory, agents) in [
            (
                project.join(".opencode"),
                json!({"shared":{"prompt":"project"}}),
            ),
            (
                custom.clone(),
                json!({"shared":{"prompt":"custom"},"custom":{"prompt":"custom"}}),
            ),
            (
                default,
                json!({"custom":{"prompt":"default"},"default":{"prompt":"default"}}),
            ),
        ] {
            fs::write(
                directory.join("opencode.json"),
                json!({"agents":agents}).to_string(),
            )
            .expect("config");
        }
        let _custom =
            crate::test_support::EnvOverride::set("OPENCODE_CONFIG_DIR", &custom.to_string_lossy());
        let _xdg = crate::test_support::EnvOverride::set("XDG_CONFIG_HOME", &xdg.to_string_lossy());
        let agents = read_opencode_config_agents(&project);
        assert_eq!(agents.len(), 3);
        assert_eq!(agents["shared"].prompt.as_deref(), Some("project"));
        assert_eq!(agents["custom"].prompt.as_deref(), Some("custom"));
        assert_eq!(agents["default"].prompt.as_deref(), Some("default"));
    }
    #[test]
    fn inline_agents_precede_definitions_and_project_precedes_global_configs() {
        // Given project/global JSONC configs, a definition, and invalid data.
        let dir = tempfile::tempdir().expect("fixture");
        let project = dir.path().join("project");
        let global = dir.path().join("global");
        for path in [&project, &global] {
            fs::create_dir_all(path).expect("directory");
        }
        fs::write(
            project.join("agent.md"),
            "---\nname: duplicate\n---\nDefinition",
        )
        .expect("definition");
        fs::write(
            project.join("second.json"),
            r#"{"name":"second","prompt":"second"}"#,
        )
        .expect("definition");
        fs::write(
            project.join("opencode.jsonc"),
            format!(
                "// comment\n{}",
                json!({"agents":{
            "duplicate":{"prompt":"inline","model":"opus","tools":["Read"],"mode":"primary"},
            "invalid":false,"defaulted":{"tools":"Bash, Edit","mode":"invalid"}
        },"agent_definitions":["agent.md","second.json",42,"../../escaped.md"]})
            ),
        )
        .expect("project config");
        fs::write(
            global.join("opencode.json"),
            json!({"agent":{"duplicate":{"prompt":"global"},"global":{"prompt":"global"}}})
                .to_string(),
        )
        .expect("global config");
        // When reading roots in precedence order.
        let result = read_config_agents_from_roots(&project, &[project.clone(), global]);
        // Then inline/nearest wins and valid definitions and fallback aliases merge.
        assert_eq!(result.len(), 4);
        assert_eq!(result["duplicate"].prompt.as_deref(), Some("inline"));
        assert_eq!(result["duplicate"].mode, Some(AgentMode::Primary));
        assert_eq!(
            result["duplicate"].model,
            Some(AgentModel::String("anthropic/claude-opus-5-5".into()))
        );
        assert_eq!(result["defaulted"].mode, Some(AgentMode::Subagent));
        assert_eq!(result["defaulted"].tools.as_ref().expect("tools").len(), 2);
        assert_eq!(result["second"].prompt.as_deref(), Some("second"));
        assert!(!result.contains_key("invalid"));
    }
    #[test]
    fn missing_and_malformed_configs_return_empty_records() {
        // Given a missing config followed by malformed JSON.
        let dir = tempfile::tempdir().expect("fixture");
        fs::write(dir.path().join("opencode.json"), "{ invalid").expect("config");
        // When reading the explicit root, then neither input creates agents.
        assert!(read_config_agents_from_roots(dir.path(), &[dir.path().to_path_buf()]).is_empty());
    }

    #[test]
    fn modes_defaults_aliases_and_string_definitions_are_supported() {
        let dir = tempfile::tempdir().expect("fixture");
        fs::write(
            dir.path().join("definition.json"),
            json!({"name":"definition","description":"file","prompt":"file prompt"}).to_string(),
        )
        .expect("definition");
        fs::write(
            dir.path().join("opencode.json"),
            json!({"agent":{
            "all":{"mode":"all","model":"sonnet"},
            "default":{},"bad-mode":{"mode":"unknown","description":null,"prompt":false},
            "invalid":null
        },"agent_definitions":"definition.json"})
            .to_string(),
        )
        .expect("config");
        let agents = read_config_agents_from_roots(dir.path(), &[dir.path().to_path_buf()]);
        assert_eq!(agents.len(), 4);
        assert_eq!(agents["all"].mode, Some(AgentMode::All));
        assert_eq!(
            agents["all"].model,
            Some(AgentModel::String("anthropic/claude-sonnet-5-5".into()))
        );
        assert_eq!(agents["default"].mode, Some(AgentMode::Subagent));
        assert_eq!(agents["default"].prompt.as_deref(), Some(""));
        assert!(agents["default"].tools.is_none());
        assert_eq!(agents["bad-mode"].prompt.as_deref(), Some(""));
        assert_eq!(agents["definition"].prompt.as_deref(), Some("file prompt"));
    }

    #[test]
    fn json_precedes_jsonc_and_unreadable_config_is_ignored() {
        let dir = tempfile::tempdir().expect("fixture");
        fs::write(
            dir.path().join("opencode.json"),
            json!({"agents":{"same":{"prompt":"json"}}}).to_string(),
        )
        .expect("json");
        fs::write(
            dir.path().join("opencode.jsonc"),
            json!({"agents":{"same":{"prompt":"jsonc"},"second":{"prompt":"jsonc"}}}).to_string(),
        )
        .expect("jsonc");
        let roots = [dir.path().to_path_buf()];
        let agents = read_config_agents_from_roots(dir.path(), &roots);
        assert_eq!(agents["same"].prompt.as_deref(), Some("json"));
        assert_eq!(agents["second"].prompt.as_deref(), Some("jsonc"));
        fs::remove_file(dir.path().join("opencode.json")).expect("remove");
        fs::remove_file(dir.path().join("opencode.jsonc")).expect("remove");
        fs::create_dir(dir.path().join("opencode.json")).expect("unreadable file fixture");
        assert!(read_config_agents_from_roots(dir.path(), &roots).is_empty());
    }
}
