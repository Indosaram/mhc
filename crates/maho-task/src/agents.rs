//! Agent definitions: bundled agents, markdown agent files with YAML frontmatter, and the
//! `agents` section of the harness configuration (ported from the oh-my-pi task model,
//! `packages/coding-agent/src/task/{agents,discovery}.ts` and `discovery/helpers.ts`
//! `parseAgentFields`).
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Where an agent definition came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentSource {
    Bundled,
    User,
    Project,
    Config,
}

impl AgentSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Bundled => "bundled",
            Self::User => "user",
            Self::Project => "project",
            Self::Config => "config",
        }
    }
}

/// One agent a task can run as.
#[derive(Clone, Debug, PartialEq)]
pub struct AgentDefinition {
    pub name: String,
    pub description: String,
    /// Appended to the child's system prompt.
    pub system_prompt: String,
    /// Tool allowlist; `None` keeps the child's default tools.
    pub tools: Option<Vec<String>>,
    /// Tools removed from the child.
    pub disallowed_tools: Vec<String>,
    /// Model references in preference order (first = primary, rest = fallbacks).
    pub models: Vec<String>,
    pub thinking_level: Option<String>,
    /// Runs in the background unless the call says otherwise.
    pub background: bool,
    pub read_only: bool,
    pub source: AgentSource,
    pub file_path: Option<PathBuf>,
}

/// The default agent of the `task` tool.
pub const DEFAULT_AGENT: &str = "task";

const BUNDLED: &[(&str, &str)] = &[
    ("embedded:task.md", include_str!("agents/task.md")),
    ("embedded:scout.md", include_str!("agents/scout.md")),
];

/// Names a definition may not use: they are reserved for the main session and anonymous children.
const RESERVED_NAMES: &[&str] = &["main", "sub"];

/// Splits `---\n<yaml>\n---\n<body>`; text without frontmatter is all body.
pub fn split_frontmatter(content: &str) -> (Option<&str>, &str) {
    let text = content.strip_prefix('\u{feff}').unwrap_or(content);
    let Some(rest) = text
        .strip_prefix("---\n")
        .or_else(|| text.strip_prefix("---\r\n"))
    else {
        return (None, text);
    };
    let mut offset = 0;
    for line in rest.split_inclusive('\n') {
        if line.trim_end() == "---" {
            let yaml = &rest[..offset];
            let body = &rest[offset + line.len()..];
            return (Some(yaml), body);
        }
        offset += line.len();
    }
    (None, text)
}

/// A list given as a YAML/JSON array or a comma-separated string.
fn list_or_csv(value: Option<&Value>) -> Option<Vec<String>> {
    match value? {
        Value::Array(items) => Some(
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(str::to_owned)
                .collect(),
        ),
        Value::String(text) => Some(
            text.split(',')
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(str::to_owned)
                .collect(),
        ),
        _ => None,
    }
}

fn bool_field(fields: &Value, keys: &[&str]) -> bool {
    keys.iter().any(|key| match fields.get(key) {
        Some(Value::Bool(flag)) => *flag,
        Some(Value::String(text)) => text.eq_ignore_ascii_case("true"),
        _ => false,
    })
}

fn str_field<'a>(fields: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|key| fields.get(key).and_then(Value::as_str))
        .map(str::trim)
        .filter(|text| !text.is_empty())
}

/// Model references from a `model` field (string, CSV or list) or a `models` list whose entries
/// are strings or `{ model }` objects.
pub fn model_list(fields: &Value) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(models) = fields.get("models").and_then(Value::as_array) {
        for entry in models {
            let model = entry
                .as_str()
                .or_else(|| entry.get("model").and_then(Value::as_str));
            if let Some(model) = model.map(str::trim).filter(|m| !m.is_empty()) {
                out.push(model.to_owned());
            }
        }
    }
    if out.is_empty() {
        out = list_or_csv(fields.get("model")).unwrap_or_default();
    }
    out
}

/// Parses frontmatter fields into a definition; `None` when `name`/`description` is missing or
/// the name is reserved.
pub fn agent_from_fields(
    fields: &Value,
    body: &str,
    source: AgentSource,
    file_path: Option<PathBuf>,
) -> Option<AgentDefinition> {
    let name = str_field(fields, &["name"])?;
    let description = str_field(fields, &["description"])?;
    if RESERVED_NAMES.contains(&name.to_ascii_lowercase().as_str()) {
        return None;
    }
    // Only an absent `tools` keeps the defaults; a malformed one must not widen access.
    let tools = match fields.get("tools") {
        None | Some(Value::Null) => None,
        Some(value) => Some(list_or_csv(Some(value)).unwrap_or_default()),
    };
    Some(AgentDefinition {
        name: name.to_owned(),
        description: description.to_owned(),
        system_prompt: body.trim().to_owned(),
        tools,
        disallowed_tools: list_or_csv(fields.get("disallowed-tools"))
            .or_else(|| list_or_csv(fields.get("disallowed_tools")))
            .unwrap_or_default(),
        models: model_list(fields),
        thinking_level: str_field(fields, &["thinking-level", "thinkingLevel", "thinking"])
            .map(str::to_owned),
        background: bool_field(fields, &["background"]),
        read_only: bool_field(fields, &["read-only", "readOnly"]),
        source,
        file_path,
    })
}

/// Parses one agent markdown file.
pub fn parse_agent(
    content: &str,
    source: AgentSource,
    file_path: Option<PathBuf>,
) -> Option<AgentDefinition> {
    let (yaml, body) = split_frontmatter(content);
    let fields: Value = serde_yaml::from_str(yaml?).ok()?;
    agent_from_fields(&fields, body, source, file_path)
}

/// The bundled agents (`task`, `scout`).
pub fn bundled_agents() -> Vec<AgentDefinition> {
    BUNDLED
        .iter()
        .filter_map(|(path, content)| {
            parse_agent(content, AgentSource::Bundled, Some(PathBuf::from(path)))
        })
        .collect()
}

/// `*.md` agents in `dir`, sorted by file name; unreadable or invalid files are skipped.
pub fn load_agents_from_dir(dir: &Path, source: AgentSource) -> Vec<AgentDefinition> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "md") && path.is_file())
        .collect();
    files.sort();
    files
        .into_iter()
        .filter_map(|path| {
            let content = std::fs::read_to_string(&path).ok()?;
            parse_agent(&content, source, Some(path))
        })
        .collect()
}

/// Nearest `<ancestor>/.omo/agents` directory of `cwd`.
pub fn project_agents_dir(cwd: &Path) -> Option<PathBuf> {
    cwd.ancestors()
        .map(|dir| dir.join(".omo").join("agents"))
        .find(|dir| dir.is_dir())
}

/// Applies the `agents` configuration section: `disable: true` removes an agent; model, tool,
/// prompt and background fields override; an unknown name with both `description` and `prompt`
/// defines a new agent.
pub fn apply_config(agents: &mut Vec<AgentDefinition>, section: Option<&Value>) {
    let Some(Value::Object(entries)) = section else {
        return;
    };
    for (name, fields) in entries {
        if !fields.is_object() {
            continue;
        }
        let position = agents.iter().position(|agent| &agent.name == name);
        if bool_field(fields, &["disable"]) {
            if let Some(index) = position {
                agents.remove(index);
            }
            continue;
        }
        let index = match position {
            Some(index) => index,
            None => {
                let (Some(description), Some(prompt)) = (
                    str_field(fields, &["description"]),
                    str_field(fields, &["prompt"]),
                ) else {
                    continue;
                };
                if RESERVED_NAMES.contains(&name.to_ascii_lowercase().as_str()) {
                    continue;
                }
                agents.push(AgentDefinition {
                    name: name.clone(),
                    description: description.to_owned(),
                    system_prompt: prompt.to_owned(),
                    tools: None,
                    disallowed_tools: Vec::new(),
                    models: Vec::new(),
                    thinking_level: None,
                    background: false,
                    read_only: false,
                    source: AgentSource::Config,
                    file_path: None,
                });
                agents.len() - 1
            }
        };
        let agent = &mut agents[index];
        if let Some(description) = str_field(fields, &["description"]) {
            description.clone_into(&mut agent.description);
        }
        if agent.source != AgentSource::Config
            && let Some(prompt) = str_field(fields, &["prompt"])
        {
            prompt.clone_into(&mut agent.system_prompt);
        }
        let models = model_list(fields);
        if !models.is_empty() {
            agent.models = models;
        }
        if let Some(level) = str_field(fields, &["reasoning", "variant", "reasoningEffort"]) {
            agent.thinking_level = Some(level.to_owned());
        }
        if let Some(Value::Object(tools)) = fields.get("tools") {
            for (tool, enabled) in tools {
                if enabled == &Value::Bool(false) && !agent.disallowed_tools.contains(tool) {
                    agent.disallowed_tools.push(tool.clone());
                }
            }
        }
        if let Some(extra) = list_or_csv(fields.get("disallowed_tools")) {
            for tool in extra {
                if !agent.disallowed_tools.contains(&tool) {
                    agent.disallowed_tools.push(tool);
                }
            }
        }
        if let Some(Value::Bool(background)) = fields.get("background") {
            agent.background = *background;
        }
        if let Some(append) = str_field(fields, &["prompt_append"]) {
            if !agent.system_prompt.is_empty() {
                agent.system_prompt.push_str("\n\n");
            }
            agent.system_prompt.push_str(append);
        }
    }
}

/// Discovery result.
#[derive(Clone, Debug, Default)]
pub struct Discovery {
    pub agents: Vec<AgentDefinition>,
    /// Directories searched, highest precedence first.
    pub searched: Vec<PathBuf>,
}

impl Discovery {
    pub fn get(&self, name: &str) -> Option<&AgentDefinition> {
        self.agents.iter().find(|agent| agent.name == name)
    }
    pub fn names(&self) -> Vec<String> {
        self.agents.iter().map(|agent| agent.name.clone()).collect()
    }
}

/// Agents for `cwd`: project `.omo/agents`, then `<agent dir>/agents`, then bundled (the first
/// definition of a name wins), then the configuration `agents` section applied on top.
pub fn discover(cwd: &Path, agent_dir: Option<&Path>, config: Option<&Value>) -> Discovery {
    let mut ordered: Vec<(PathBuf, AgentSource)> = Vec::new();
    if let Some(project) = project_agents_dir(cwd) {
        ordered.push((project, AgentSource::Project));
    }
    if let Some(dir) = agent_dir {
        ordered.push((dir.join("agents"), AgentSource::User));
    }
    let mut seen: BTreeMap<String, ()> = BTreeMap::new();
    let mut agents = Vec::new();
    let loaded = ordered
        .iter()
        .flat_map(|(dir, source)| load_agents_from_dir(dir, *source));
    for agent in loaded.chain(bundled_agents()) {
        if seen.insert(agent.name.clone(), ()).is_none() {
            agents.push(agent);
        }
    }
    apply_config(&mut agents, config);
    Discovery {
        agents,
        searched: ordered.into_iter().map(|(dir, _)| dir).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn bundled_agents_parse() {
        let agents = bundled_agents();
        let names: Vec<&str> = agents.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["task", "scout"]);
        let scout = &agents[1];
        assert!(scout.read_only);
        assert_eq!(
            scout.tools.as_deref(),
            Some(&["read", "grep", "find", "ls"].map(String::from)[..])
        );
        assert!(agents[0].tools.is_none());
        assert!(agents[0].system_prompt.starts_with("Worker agent"));
    }

    #[test]
    fn frontmatter_fields() {
        let agent = parse_agent(
            "---\nname: docs\ndescription: Writes docs\ntools: [read, write]\nmodel: a/b, c/d\nthinking-level: high\nbackground: true\n---\nBody\n",
            AgentSource::Project,
            None,
        )
        .expect("agent");
        assert_eq!(agent.models, ["a/b", "c/d"]);
        assert_eq!(agent.thinking_level.as_deref(), Some("high"));
        assert!(agent.background);
        assert_eq!(agent.system_prompt, "Body");
        assert!(parse_agent("---\nname: main\ndescription: x\n---\n", AgentSource::User, None).is_none());
        assert!(parse_agent("no frontmatter", AgentSource::User, None).is_none());
        assert!(parse_agent("---\nname: x\n---\n", AgentSource::User, None).is_none());
    }

    #[test]
    fn malformed_tools_do_not_widen_access() {
        let agent = parse_agent(
            "---\nname: x\ndescription: y\ntools: 3\n---\n",
            AgentSource::User,
            None,
        )
        .expect("agent");
        assert_eq!(agent.tools, Some(Vec::new()));
    }

    #[test]
    fn discovery_precedence_and_config() {
        let root = tempfile::tempdir().expect("tmp");
        let project = root.path().join("repo");
        let nested = project.join("src");
        std::fs::create_dir_all(project.join(".omo/agents")).expect("dir");
        std::fs::create_dir_all(&nested).expect("dir");
        std::fs::write(
            project.join(".omo/agents/task.md"),
            "---\nname: task\ndescription: project task\n---\nP",
        )
        .expect("write");
        let agent_dir = root.path().join("agent");
        std::fs::create_dir_all(agent_dir.join("agents")).expect("dir");
        std::fs::write(
            agent_dir.join("agents/task.md"),
            "---\nname: task\ndescription: user task\n---\nU",
        )
        .expect("write");
        let config = json!({
            "scout": {"disable": true},
            "task": {"models": [{"model": "p/m"}, "q/n"], "tools": {"bash": false}},
            "writer": {"description": "Writes", "prompt": "Write well"},
            "only-model": {"model": "x/y"}
        });
        let found = discover(&nested, Some(&agent_dir), Some(&config));
        assert_eq!(found.names(), ["task", "writer"]);
        let task = found.get("task").expect("task");
        assert_eq!(task.description, "project task");
        assert_eq!(task.source, AgentSource::Project);
        assert_eq!(task.models, ["p/m", "q/n"]);
        assert_eq!(task.disallowed_tools, ["bash"]);
        assert_eq!(found.get("writer").expect("writer").source, AgentSource::Config);
        assert_eq!(found.searched.len(), 2);
    }
}
