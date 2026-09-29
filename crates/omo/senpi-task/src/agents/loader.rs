//! Agent loading: pi-compatible markdown locations, then programmatic registrations, then the
//! `omo.json` overlay (`agents/{loader,paths,markdown,registry,omo-overlay,omo-config-agents}.ts`).
//!
//! The final `omo.json` overlay is intentional: user/project config is the final authority so a
//! checked-in or local `omo.json` can override component defaults without code changes.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex, PoisonError};

use omo_config_core::{
    LoadOmoConfigOptions, load_omo_config, process_env, resolve_model_references,
};
use serde_json::Value;
use utils::frontmatter::{FrontmatterMode, parse_frontmatter};

use super::schema::parse_model_entry;
use super::schema::parse_raw_agent_definition;
use super::tools::normalize_tool_rules;
use super::types::{
    AgentDefinition, AgentLoaderDiagnostic, AgentLoaderDiagnosticKind, LoadAgentsOptions,
    LoadAgentsResult,
};

const AGENT_SUBDIRECTORIES: [&str; 2] = ["agent", "agents"];

/// Programmatic agent registrations, overlaid after markdown files and before `omo.json`.
#[derive(Debug, Default)]
pub struct AgentRegistry {
    agents: Mutex<Vec<AgentDefinition>>,
}

impl AgentRegistry {
    pub const fn new() -> Self {
        Self {
            agents: Mutex::new(Vec::new()),
        }
    }

    pub fn register(&self, definition: AgentDefinition) -> AgentDefinition {
        let mut agents = self.agents.lock().unwrap_or_else(PoisonError::into_inner);
        agents.retain(|existing| existing.name != definition.name);
        agents.push(definition.clone());
        definition
    }

    pub fn definitions(&self) -> Vec<AgentDefinition> {
        self.agents
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

static REGISTERED_AGENTS: LazyLock<AgentRegistry> = LazyLock::new(AgentRegistry::new);

/// Registers into the process-wide registry that [`load_agents`] overlays.
pub fn register_agent(definition: AgentDefinition) -> AgentDefinition {
    REGISTERED_AGENTS.register(definition)
}

pub fn load_agents(options: &LoadAgentsOptions) -> LoadAgentsResult {
    load_agents_with_registry(options, &REGISTERED_AGENTS)
}

/// [`load_agents`] against an explicit registry instead of the process-wide one.
pub fn load_agents_with_registry(
    options: &LoadAgentsOptions,
    registry: &AgentRegistry,
) -> LoadAgentsResult {
    let cwd = std::env::current_dir().unwrap_or_default();
    let absolute = |path: &str| std::path::absolute(path).unwrap_or_else(|_| PathBuf::from(path));
    let home_dir = options
        .home_dir
        .clone()
        .or_else(|| std::env::var("HOME").ok())
        .map_or_else(|| cwd.clone(), |home| absolute(&home));
    let project_dir = options
        .project_dir
        .as_deref()
        .map_or_else(|| cwd.clone(), absolute);

    let mut result = LoadAgentsResult::default();
    for location in agent_definition_locations(&home_dir, &project_dir) {
        let (files, diagnostics) = list_markdown_agent_files(&location);
        result.diagnostics.extend(diagnostics);
        for path in files {
            match load_markdown_agent(&path) {
                Ok(agent) => overlay_agent(&mut result.agents, agent),
                Err(diagnostic) => result.diagnostics.push(diagnostic),
            }
        }
    }

    for definition in registry.definitions() {
        overlay_agent(&mut result.agents, definition);
    }

    let (agents, diagnostics) = load_omo_agent_overlays(options, &home_dir, &project_dir);
    result.diagnostics.extend(diagnostics);
    for definition in agents {
        overlay_agent(&mut result.agents, definition);
    }
    result
}

fn overlay_agent(agents: &mut Vec<(String, AgentDefinition)>, definition: AgentDefinition) {
    match agents.iter_mut().find(|(name, _)| *name == definition.name) {
        Some((_, existing)) => *existing = existing.clone().overlaid_with(definition),
        None => agents.push((definition.name.clone(), definition)),
    }
}

fn agent_definition_locations(home_dir: &Path, project_dir: &Path) -> Vec<PathBuf> {
    vec![
        home_dir.join(".pi").join("agent"),
        home_dir.join(".senpi").join("agent"),
        home_dir.join(".senpi").join("agents"),
        project_dir.join(".pi"),
        project_dir.join(".senpi"),
        project_dir.join(".senpi").join("agents"),
    ]
}

fn display(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn read_diagnostic(path: &Path, message: String) -> AgentLoaderDiagnostic {
    AgentLoaderDiagnostic {
        kind: AgentLoaderDiagnosticKind::Read,
        path: display(path),
        message,
        issue_paths: None,
    }
}

fn symlink_diagnostic(path: &Path) -> AgentLoaderDiagnostic {
    let shown = display(path);
    read_diagnostic(
        path,
        format!("Refusing to follow symlinked agent path {shown}"),
    )
}

fn io_failure(path: &Path, operation: &str, error: &io::Error) -> AgentLoaderDiagnostic {
    let shown = display(path);
    let code = match error.kind() {
        io::ErrorKind::NotFound => " (ENOENT)",
        io::ErrorKind::PermissionDenied => " (EACCES)",
        io::ErrorKind::NotADirectory => " (ENOTDIR)",
        _ => "",
    };
    read_diagnostic(
        path,
        format!("Failed to {operation} agent directory {shown}{code}: {error}"),
    )
}

/// `Ok(true)` when `dir` is a real directory, `Ok(false)` when missing and ignorable.
fn inspect_directory(dir: &Path, ignore_missing: bool) -> Result<bool, AgentLoaderDiagnostic> {
    match fs::symlink_metadata(dir) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(symlink_diagnostic(dir)),
        Ok(metadata) if !metadata.is_dir() => {
            let shown = display(dir);
            Err(read_diagnostic(
                dir,
                format!("Agent scan path is not a directory: {shown}"),
            ))
        }
        Ok(_) => Ok(true),
        Err(error) if ignore_missing && error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(io_failure(dir, "inspect", &error)),
    }
}

fn list_markdown_agent_files(location: &Path) -> (Vec<PathBuf>, Vec<AgentLoaderDiagnostic>) {
    match inspect_directory(location, true) {
        Ok(true) => {}
        Ok(false) => return (Vec::new(), Vec::new()),
        Err(diagnostic) => return (Vec::new(), vec![diagnostic]),
    }
    let mut files = Vec::new();
    let mut diagnostics = Vec::new();
    for subdir in AGENT_SUBDIRECTORIES {
        list_markdown_files(&location.join(subdir), true, &mut files, &mut diagnostics);
    }
    (files, diagnostics)
}

fn list_markdown_files(
    dir: &Path,
    ignore_missing: bool,
    files: &mut Vec<PathBuf>,
    diagnostics: &mut Vec<AgentLoaderDiagnostic>,
) {
    match inspect_directory(dir, ignore_missing) {
        Ok(true) => {}
        Ok(false) => return,
        Err(diagnostic) => return diagnostics.push(diagnostic),
    }
    let mut entries = match fs::read_dir(dir).and_then(Iterator::collect::<io::Result<Vec<_>>>) {
        Ok(entries) => entries,
        Err(error) => return diagnostics.push(io_failure(dir, "scan", &error)),
    };
    entries.sort_by_key(|entry| {
        let name = entry.file_name().to_string_lossy().into_owned();
        (name.to_lowercase(), name)
    });
    for entry in entries {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_symlink() {
            diagnostics.push(symlink_diagnostic(&path));
        } else if file_type.is_dir() {
            list_markdown_files(&path, false, files, diagnostics);
        } else if file_type.is_file() && path.extension().is_some_and(|extension| extension == "md")
        {
            files.push(path);
        }
    }
}

fn load_markdown_agent(path: &Path) -> Result<AgentDefinition, AgentLoaderDiagnostic> {
    let shown = display(path);
    let content = fs::read_to_string(path)
        .map_err(|error| read_diagnostic(path, format!("Failed to read {shown}: {error}")))?;
    let parsed = parse_frontmatter(&content, FrontmatterMode::Default);
    if parsed.parse_error {
        return Err(AgentLoaderDiagnostic {
            kind: AgentLoaderDiagnosticKind::Frontmatter,
            path: shown.clone(),
            message: format!("Malformed YAML frontmatter in {shown}"),
            issue_paths: None,
        });
    }
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy())
        .unwrap_or_default();
    let name = name.strip_suffix(".md").unwrap_or(&name).to_string();
    parse_raw_agent_definition(&name, &parsed.data, Some(parsed.body)).map_err(|issue_paths| {
        AgentLoaderDiagnostic {
            kind: AgentLoaderDiagnosticKind::Validation,
            path: shown.clone(),
            message: format!(
                "Invalid agent frontmatter in {shown}: {}",
                issue_paths.join(", ")
            ),
            issue_paths: Some(issue_paths),
        }
    })
}

fn load_omo_agent_overlays(
    options: &LoadAgentsOptions,
    home_dir: &Path,
    project_dir: &Path,
) -> (Vec<AgentDefinition>, Vec<AgentLoaderDiagnostic>) {
    let mut env = process_env();
    env.extend(options.env.clone().unwrap_or_default());
    let home = display(home_dir);
    env.insert(
        "APPDATA".to_string(),
        display(&home_dir.join("AppData").join("Roaming")),
    );
    env.insert("HOME".to_string(), home.clone());
    env.insert("USERPROFILE".to_string(), home);
    env.insert(
        "XDG_CONFIG_HOME".to_string(),
        display(&home_dir.join(".config")),
    );
    let loaded = load_omo_config(&LoadOmoConfigOptions {
        cwd: Some(display(project_dir)),
        env: Some(env),
        file_system: None,
        harness: Some("senpi".to_string()),
        platform: None,
        profile: None,
    });
    let resolved = resolve_model_references(&Value::Object(loaded.config));
    let mut diagnostics: Vec<AgentLoaderDiagnostic> = loaded
        .diagnostics
        .into_iter()
        .map(|diagnostic| AgentLoaderDiagnostic {
            kind: match diagnostic.kind {
                "parse" => AgentLoaderDiagnosticKind::ConfigParse,
                "read" => AgentLoaderDiagnosticKind::Read,
                _ => AgentLoaderDiagnosticKind::Validation,
            },
            path: diagnostic.path,
            message: diagnostic.message,
            issue_paths: (!diagnostic.issue_paths.is_empty()).then_some(diagnostic.issue_paths),
        })
        .collect();
    diagnostics.extend(
        resolved
            .diagnostics
            .into_iter()
            .map(|diagnostic| AgentLoaderDiagnostic {
                kind: AgentLoaderDiagnosticKind::Validation,
                path: diagnostic.path,
                message: diagnostic.message,
                issue_paths: None,
            }),
    );
    let agents = map_omo_config_agents(&resolved.view)
        .into_iter()
        .map(|(_, definition)| definition)
        .collect();
    (agents, diagnostics)
}

/// Bridges already-loaded `omo.json` agents (snake_case keys, `{ name: bool }` tools) onto
/// [`AgentDefinition`]s keyed by record name; fields the source omits stay absent.
pub fn map_omo_config_agents(config: &Value) -> Vec<(String, AgentDefinition)> {
    let Some(agents) = config.get("agents").and_then(Value::as_object) else {
        return Vec::new();
    };
    agents
        .iter()
        .map(|(name, def)| (name.clone(), omo_agent_definition(name, def)))
        .collect()
}

fn omo_agent_definition(name: &str, def: &Value) -> AgentDefinition {
    let text = |key: &str| def.get(key).and_then(Value::as_str).map(str::to_string);
    let strings = |key: &str| {
        def.get(key).and_then(Value::as_array).map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
    };
    let flag = |key: &str| def.get(key).and_then(Value::as_bool);
    AgentDefinition {
        name: name.to_string(),
        description: text("description"),
        prompt: text("prompt"),
        mode: None,
        model: text("model"),
        models: def
            .get("models")
            .and_then(Value::as_array)
            .map(|entries| entries.iter().filter_map(parse_model_entry).collect()),
        variant: text("variant"),
        reasoning_effort: text("reasoning").or_else(|| text("reasoningEffort")),
        temperature: def.get("temperature").and_then(Value::as_f64),
        tools: normalize_tool_rules(def.get("tools")),
        disable: flag("disable"),
        background: flag("background"),
        execution_mode: text("execution_mode"),
        allowed_subagents: strings("allowed_subagents"),
        disallowed_tools: strings("disallowed_tools"),
        max_depth: def.get("max_depth").and_then(Value::as_u64),
        max_turns: def.get("max_turns").and_then(Value::as_u64),
    }
}
