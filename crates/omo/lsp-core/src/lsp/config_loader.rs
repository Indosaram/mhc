//! Port of TS `config-loader.ts`: merges project, user and builtin LSP server configs.

use crate::lsp::server_definitions::BUILTIN_SERVERS;
use crate::lsp::server_definitions::builtin_server;
use crate::lsp::types::ResolvedServer;
use crate::request_context::LspRequestContextUnavailableError;
use crate::request_context::lsp_request_context;
use serde_json::Map;
use serde_json::Value;
use std::collections::BTreeMap;
use std::collections::HashSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConfigSource {
    Project,
    User,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ServerSource {
    Project,
    User,
    Builtin,
}

impl ServerSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Project => "project",
            Self::User => "user",
            Self::Builtin => "builtin",
        }
    }

    fn order(self) -> u8 {
        match self {
            Self::Project => 0,
            Self::User => 1,
            Self::Builtin => 2,
        }
    }
}

impl From<ConfigSource> for ServerSource {
    fn from(source: ConfigSource) -> Self {
        match source {
            ConfigSource::Project => Self::Project,
            ConfigSource::User => Self::User,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ServerWithSource {
    pub server: ResolvedServer,
    pub source: ServerSource,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigPaths {
    pub project: String,
    pub user: String,
}

#[derive(Debug, Clone, Default)]
struct LspEntry {
    disabled: Option<bool>,
    command: Option<Vec<String>>,
    extensions: Option<Vec<String>>,
    priority: Option<f64>,
    env: Option<BTreeMap<String, String>>,
    initialization: Option<Map<String, Value>>,
}

/// Parsed config file: the `lsp` record in declaration order (if present).
type ConfigJson = Option<Map<String, Value>>;

/// TS `getConfigPaths`.
pub fn get_config_paths() -> Result<ConfigPaths, LspRequestContextUnavailableError> {
    let context = lsp_request_context()?;
    Ok(ConfigPaths {
        project: context
            .project_config_paths
            .first()
            .cloned()
            .unwrap_or_default(),
        user: context.user_config_path,
    })
}

fn load_json_file(path: &str) -> Option<ConfigJson> {
    let text = std::fs::read_to_string(path).ok()?;
    let parsed: Value = serde_json::from_str(&text).ok()?;
    let record = parsed.as_object()?;
    match record.get("lsp") {
        None => Some(None),
        Some(Value::Object(lsp)) => Some(Some(lsp.clone())),
        Some(_) => None,
    }
}

/// TS `loadAllConfigs`: the first loadable project config, then the user config.
fn load_all_configs() -> Result<Vec<(ConfigSource, ConfigJson)>, LspRequestContextUnavailableError>
{
    let context = lsp_request_context()?;
    let mut configs = Vec::new();
    if let Some(project) = context
        .project_config_paths
        .iter()
        .find_map(|path| load_json_file(path))
    {
        configs.push((ConfigSource::Project, project));
    }
    if let Some(user) = load_json_file(&context.user_config_path) {
        configs.push((ConfigSource::User, user));
    }
    Ok(configs)
}

/// TS `getMergedServers`: project > user > builtin, then descending priority.
pub fn get_merged_servers() -> Result<Vec<ServerWithSource>, LspRequestContextUnavailableError> {
    let configs = load_all_configs()?;
    let mut servers: Vec<ServerWithSource> = Vec::new();
    let mut disabled: HashSet<String> = HashSet::new();
    let mut seen: HashSet<String> = HashSet::new();

    for (source, config) in &configs {
        let Some(lsp) = config else { continue };
        for (id, raw) in lsp {
            let Some(entry) = parse_lsp_entry(raw) else {
                continue;
            };
            if entry.disabled == Some(true) {
                disabled.insert(id.clone());
                continue;
            }
            if seen.contains(id) {
                continue;
            }
            let Some(server) = create_server_from_entry(id, &entry, *source) else {
                continue;
            };
            servers.push(server);
            seen.insert(id.clone());
        }
    }

    for builtin in BUILTIN_SERVERS {
        if disabled.contains(builtin.id) || seen.contains(builtin.id) {
            continue;
        }
        servers.push(ServerWithSource {
            server: ResolvedServer {
                id: builtin.id.to_string(),
                command: builtin.command_vec(),
                extensions: builtin.extensions_vec(),
                priority: -100.0,
                env: None,
                initialization: None,
            },
            source: ServerSource::Builtin,
        });
    }

    // Stable sort, matching Array.prototype.sort.
    servers.sort_by(|a, b| {
        a.source.order().cmp(&b.source.order()).then_with(|| {
            b.server
                .priority
                .partial_cmp(&a.server.priority)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
    });
    Ok(servers)
}

fn create_server_from_entry(
    id: &str,
    entry: &LspEntry,
    source: ConfigSource,
) -> Option<ServerWithSource> {
    let builtin = builtin_server(id);
    let priority = entry.priority.unwrap_or(0.0);
    let make = |command: Vec<String>, extensions: Vec<String>, with_env: bool| ServerWithSource {
        server: ResolvedServer {
            id: id.to_string(),
            command,
            extensions,
            priority,
            env: if with_env { entry.env.clone() } else { None },
            initialization: entry.initialization.clone(),
        },
        source: source.into(),
    };
    if source == ConfigSource::Project {
        // Project configs may not override the command: only builtin ids are accepted.
        let builtin = builtin?;
        return Some(make(
            builtin.command_vec(),
            entry
                .extensions
                .clone()
                .unwrap_or_else(|| builtin.extensions_vec()),
            false,
        ));
    }
    if let (Some(command), Some(extensions)) = (&entry.command, &entry.extensions) {
        return Some(make(command.clone(), extensions.clone(), true));
    }
    let builtin = builtin?;
    Some(make(
        entry
            .command
            .clone()
            .unwrap_or_else(|| builtin.command_vec()),
        entry
            .extensions
            .clone()
            .unwrap_or_else(|| builtin.extensions_vec()),
        true,
    ))
}

fn parse_lsp_entry(value: &Value) -> Option<LspEntry> {
    let record = value.as_object()?;
    let mut entry = LspEntry::default();
    if let Some(disabled) = record.get("disabled") {
        entry.disabled = Some(disabled.as_bool()?);
    }
    if let Some(command) = record.get("command") {
        entry.command = Some(string_array(command)?);
    }
    if let Some(extensions) = record.get("extensions") {
        entry.extensions = Some(string_array(extensions)?);
    }
    if let Some(priority) = record.get("priority") {
        entry.priority = Some(priority.as_f64()?);
    }
    if let Some(env) = record.get("env") {
        let env = env.as_object()?;
        let mut out = BTreeMap::new();
        for (key, value) in env {
            out.insert(key.clone(), value.as_str()?.to_string());
        }
        entry.env = Some(out);
    }
    if let Some(initialization) = record.get("initialization") {
        entry.initialization = Some(initialization.as_object()?.clone());
    }
    Some(entry)
}

fn string_array(value: &Value) -> Option<Vec<String>> {
    value
        .as_array()?
        .iter()
        .map(|item| item.as_str().map(str::to_string))
        .collect()
}

/// TS `getDisabledServerIds`.
pub fn get_disabled_server_ids() -> Result<Vec<String>, LspRequestContextUnavailableError> {
    let mut disabled: Vec<String> = Vec::new();
    for (_, config) in load_all_configs()? {
        let Some(lsp) = config else { continue };
        for (id, raw) in &lsp {
            if parse_lsp_entry(raw).is_some_and(|entry| entry.disabled == Some(true))
                && !disabled.contains(id)
            {
                disabled.push(id.clone());
            }
        }
    }
    Ok(disabled)
}
