use super::{agent_definitions_loader::parse_markdown_agent_file, types::*};
use crate::shared::get_claude_config_dir;
use std::{collections::BTreeMap, fs, io, path::Path};
use utils::file_utils::is_markdown_file;

pub fn load_agents_from_dir(
    directory: &Path,
    scope: AgentScope,
    anthropic_provider: Option<&str>,
) -> io::Result<Vec<LoadedAgent>> {
    if !directory.exists() {
        return Ok(Vec::new());
    }
    let mut agents = Vec::new();
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if !is_markdown_file(
            &entry.file_name().to_string_lossy(),
            entry.file_type()?.is_file(),
        ) {
            continue;
        }
        if let Some(agent) = parse_markdown_agent_file(&entry.path(), scope, anthropic_provider) {
            agents.push(agent);
        }
    }
    Ok(agents)
}

pub fn load_user_agents(
    anthropic_provider: Option<&str>,
) -> io::Result<BTreeMap<String, ClaudeCodeAgentConfig>> {
    let agents = load_agents_from_dir(
        &get_claude_config_dir().join("agents"),
        AgentScope::User,
        anthropic_provider,
    )?;
    Ok(agents
        .into_iter()
        .map(|agent| (agent.name, agent.config))
        .collect())
}

pub fn load_native_agents(
    cwd: &Path,
    claude_dir: &Path,
    anthropic_provider: Option<&str>,
) -> io::Result<BTreeMap<String, ClaudeCodeAgentConfig>> {
    let options = crate::PluginLoaderOptions {
        cwd: Some(cwd.to_owned()),
        claude_config_dir: Some(claude_dir.to_owned()),
        plugins_home_override: Some(claude_dir.join("plugins")),
        anthropic_provider: anthropic_provider.map(str::to_owned),
        ..Default::default()
    };
    let plugins = crate::discover_installed_plugins(&options)?;
    for error in plugins.errors {
        crate::shared::logger::log(&format!("Claude plugin {}: {}", error.plugin_key, error.error), None);
    }
    let mut agents = crate::load_plugin_agents(&plugins.plugins, anthropic_provider)?;
    for (directory, scope) in [(claude_dir.join("agents"), AgentScope::User),
        (cwd.join(".claude/agents"), AgentScope::Project)] {
        agents.extend(load_agents_from_dir(&directory, scope, anthropic_provider)?
            .into_iter().map(|agent| (agent.name, agent.config)));
    }
    Ok(agents)
}

pub fn load_project_agents(
    directory: Option<&Path>,
    anthropic_provider: Option<&str>,
) -> io::Result<BTreeMap<String, ClaudeCodeAgentConfig>> {
    let directory = match directory {
        Some(directory) => directory.to_path_buf(),
        None => std::env::current_dir()?,
    };
    let agents = load_agents_from_dir(
        &directory.join(".claude/agents"),
        AgentScope::Project,
        anthropic_provider,
    )?;
    Ok(agents
        .into_iter()
        .map(|agent| (agent.name, agent.config))
        .collect())
}

pub fn load_opencode_global_agents() -> io::Result<BTreeMap<String, ClaudeCodeAgentConfig>> {
    let mut result = BTreeMap::new();
    for root in crate::shared::get_open_code_config_dirs(&Default::default()) {
        for agent in load_agents_from_dir(&root.join("agents"), AgentScope::Opencode, None)? {
            result.entry(agent.name).or_insert(agent.config);
        }
    }
    Ok(result)
}

pub fn load_opencode_project_agents(
    directory: Option<&Path>,
) -> io::Result<BTreeMap<String, ClaudeCodeAgentConfig>> {
    let root = match directory {
        Some(root) => root.to_path_buf(),
        None => std::env::current_dir()?,
    };
    Ok(load_agents_from_dir(
        &root.join(".opencode/agents"),
        AgentScope::OpencodeProject,
        None,
    )?
    .into_iter()
    .map(|agent| (agent.name, agent.config))
    .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_agents_use_explicit_roots_and_project_precedence() -> io::Result<()> {
        // Given
        let root = tempfile::tempdir()?;
        let claude = root.path().join("claude");
        let cwd = root.path().join("project");
        for directory in [claude.join("agents"), cwd.join(".claude/agents"), cwd.join(".opencode/agents")] {
            fs::create_dir_all(directory)?;
        }
        fs::write(claude.join("agents/scout.md"), "---\ntools: Read\n---\nuser")?;
        fs::write(cwd.join(".claude/agents/scout.md"), "---\ntools: Bash\n---\nproject")?;
        fs::write(cwd.join(".opencode/agents/ignored.md"), "ignored")?;
        // When
        let agents = load_native_agents(&cwd, &claude, None)?;
        // Then
        assert_eq!(agents.len(), 1);
        assert_eq!(agents["scout"].tools, Some(BTreeMap::from([("bash".into(), true)])));
        Ok(())
    }

    #[test]
    fn global_agents_merge_custom_and_default_config_directories() {
        let root = tempfile::tempdir().expect("fixture");
        let custom = root.path().join("custom");
        let xdg = root.path().join("xdg");
        let default = xdg.join("opencode");
        for directory in [&custom, &default] {
            fs::create_dir_all(directory.join("agents")).expect("directory");
        }
        fs::write(
            custom.join("agents/shared.md"),
            "---\ntools: Read\n---\ncustom",
        )
        .expect("agent");
        fs::write(default.join("agents/shared.md"), "default").expect("agent");
        fs::write(default.join("agents/default.md"), "default").expect("agent");
        let _custom =
            crate::test_support::EnvOverride::set("OPENCODE_CONFIG_DIR", &custom.to_string_lossy());
        let _xdg = crate::test_support::EnvOverride::set("XDG_CONFIG_HOME", &xdg.to_string_lossy());
        let agents = load_opencode_global_agents().expect("global agents");
        assert_eq!(agents.len(), 2);
        assert_eq!(agents["shared"].prompt.as_deref(), Some("custom"));
        assert_eq!(agents["shared"].description.as_deref(), Some("(opencode) "));
        assert!(agents["default"].tools.is_none());
        let empty = root.path().join("empty");
        let _custom =
            crate::test_support::EnvOverride::set("OPENCODE_CONFIG_DIR", &empty.to_string_lossy());
        let _xdg =
            crate::test_support::EnvOverride::set("XDG_CONFIG_HOME", &empty.to_string_lossy());
        assert!(load_opencode_global_agents().expect("empty").is_empty());
        assert!(
            load_opencode_project_agents(Some(&empty))
                .expect("empty")
                .is_empty()
        );
    }

    #[test]
    fn opencode_project_agents_use_their_own_scope_and_directory() {
        let root = tempfile::tempdir().expect("fixture");
        let directory = root.path().join(".opencode/agents");
        fs::create_dir_all(&directory).expect("directory");
        fs::write(
            directory.join("worker.md"),
            "---\ntools: Read, Bash\n---\nWork",
        )
        .expect("agent");
        let agents = load_opencode_project_agents(Some(root.path())).expect("agents");
        assert_eq!(agents.len(), 1);
        assert_eq!(
            agents["worker"].description.as_deref(),
            Some("(opencode-project) ")
        );
        assert_eq!(agents["worker"].tools.as_ref().expect("tools").len(), 2);
    }

    #[test]
    fn project_scan_loads_markdown_only_with_filename_fallback_and_tools() {
        let root = tempfile::tempdir().expect("fixture directory");
        let directory = root.path().join(".claude/agents");
        fs::create_dir_all(&directory).expect("agents directory");
        for (name, content) in [
            (
                "named.md",
                "---\nname: named\ndescription: Test\ntools: Bash,Read,Edit\n---\nPrompt.",
            ),
            (
                "fallback.md",
                "---\ndescription: Minimal\n---\nMinimal prompt.",
            ),
            ("raw.md", "Raw prompt."),
            ("bad.txt", "Ignored"),
            ("bad.json", "{}"),
        ] {
            fs::write(directory.join(name), content).expect("fixture write");
        }
        let agents = load_project_agents(Some(root.path()), None).expect("agents");
        assert_eq!(agents.len(), 3);
        assert_eq!(
            agents["named"].description.as_deref(),
            Some("(project) Test")
        );
        assert_eq!(agents["named"].mode, Some(AgentMode::Subagent));
        assert_eq!(agents["named"].tools.as_ref().expect("tools").len(), 3);
        assert_eq!(
            agents["fallback"].prompt.as_deref(),
            Some("Minimal prompt.")
        );
        assert_eq!(agents["raw"].prompt.as_deref(), Some("Raw prompt."));
        assert!(agents["raw"].tools.is_none());
    }

    #[test]
    fn missing_agent_directories_are_empty_for_user_and_project_scopes() {
        let root = tempfile::tempdir().expect("fixture directory");
        assert!(
            load_project_agents(Some(root.path()), None)
                .expect("project agents")
                .is_empty()
        );
        assert!(
            load_agents_from_dir(&root.path().join("agents"), AgentScope::User, None)
                .expect("user agents")
                .is_empty()
        );
    }
}
