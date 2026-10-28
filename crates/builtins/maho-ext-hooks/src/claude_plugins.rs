//! Claude Code hook discovery: settings-file hooks and installed-plugin hooks handed to the hook
//! runtime. Clean-room implementation of docs/cleanroom/specs/claude-plugin-hooks.md (see
//! PROVENANCE.md); Claude Code owns `installed_plugins.json`, this module only reads it.
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::config_loader::HookConfigLoaderOptions;
use crate::diagnostics::{DiagnosticDraft, diagnostic};
use crate::plugin_manifest::{build_plugin_env, normalize_path};
use crate::schema::parse_hook_config;
use crate::types::{HookDiscoveryTiming, HookSourceMetadata, HookSourceScope, ParsedHookConfig};

/// The placeholder Claude Code plugins use in hook definitions for their install directory.
pub const PLUGIN_ROOT_PLACEHOLDER: &str = "${CLAUDE_PLUGIN_ROOT}";
const PRIMARY_MANIFEST: &str = ".claude-plugin/plugin.json";
const FALLBACK_MANIFEST: &str = "plugin.json";
const PLUGIN_HOOKS_FILE: &str = "hooks/hooks.json";

/// Where Claude Code keeps its files for this user.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClaudeLocations {
    /// `CLAUDE_CONFIG_DIR` when set and non-empty, else `<home>/.claude`.
    pub claude_dir: PathBuf,
    /// Used for `~` expansion in a plugin's `projectPath`.
    pub home: PathBuf,
}

impl ClaudeLocations {
    /// Resolve the locations from an environment lookup (`HOME`, then `USERPROFILE`, then the
    /// platform home directory).
    pub fn from_env(get: impl Fn(&str) -> Option<OsString>) -> Self {
        let non_empty = |key: &str| get(key).filter(|value| !value.is_empty());
        let home = non_empty("HOME")
            .or_else(|| non_empty("USERPROFILE"))
            .map(PathBuf::from)
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| PathBuf::from("."));
        let claude_dir = non_empty("CLAUDE_CONFIG_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".claude"));
        Self { claude_dir, home }
    }

    pub fn from_process_env() -> Self {
        Self::from_env(|key| std::env::var_os(key))
    }

    pub fn user_settings_path(&self) -> PathBuf {
        self.claude_dir.join("settings.json")
    }

    pub fn plugin_database_path(&self) -> PathBuf {
        self.claude_dir.join("plugins").join("installed_plugins.json")
    }
}

/// Install scope recorded by Claude Code for a plugin.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstallScope {
    User,
    Project,
    Local,
    Managed,
}

/// A plugin that is installed, enabled and applies to the session's working directory.
#[derive(Clone, Debug, PartialEq)]
pub struct DiscoveredPlugin {
    pub key: String,
    pub name: String,
    pub version: String,
    pub scope: InstallScope,
    pub install_path: PathBuf,
    pub manifest: Option<Value>,
    pub hooks_path: Option<PathBuf>,
}

/// A plugin that could not be loaded; discovery of the others continues.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginProblem {
    pub key: String,
    pub install_path: String,
    pub reason: String,
}

#[derive(Debug, Default)]
pub struct PluginDiscovery {
    /// Plugins in database order.
    pub plugins: Vec<DiscoveredPlugin>,
    pub problems: Vec<PluginProblem>,
    /// Non-fatal notes (malformed database, manifest or hooks file) written to the debug log.
    pub log: Vec<String>,
}

struct DatabaseEntry {
    key: String,
    scope: InstallScope,
    install_path: String,
    version: Option<String>,
    project_path: Option<String>,
}

enum JsonFile {
    Missing,
    Parsed(Value),
    Broken(String),
}

fn read_json_file(path: &Path) -> JsonFile {
    match std::fs::read(path) {
        Ok(bytes) => match serde_json::from_slice::<Value>(&bytes) {
            Ok(value) => JsonFile::Parsed(value),
            Err(error) => JsonFile::Broken(format!("malformed JSON: {error}")),
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => JsonFile::Missing,
        Err(error) => JsonFile::Broken(format!("unreadable: {error}")),
    }
}

/// Read a plugin manifest: `.claude-plugin/plugin.json`, or `plugin.json` at the root when the
/// first does not exist. `Ok(None)` when neither exists.
fn read_manifest(install_dir: &Path) -> Result<Option<Value>, String> {
    let primary = install_dir.join(PRIMARY_MANIFEST);
    let path = if primary.exists() { primary } else { install_dir.join(FALLBACK_MANIFEST) };
    match read_json_file(&path) {
        JsonFile::Missing => Ok(None),
        JsonFile::Parsed(value) => Ok(Some(value)),
        JsonFile::Broken(reason) => Err(format!("{}: {reason}", path.display())),
    }
}

fn non_blank_text(value: Option<&Value>) -> Option<&str> {
    value.and_then(Value::as_str).map(str::trim).filter(|text| !text.is_empty())
}

/// Plugin name derived from a database key, used when the manifest carries no name.
pub fn derive_plugin_name(key: &str) -> String {
    let key = key.strip_prefix("npm:").unwrap_or(key);
    let scoped = key.starts_with('@') && key.contains('/');
    let bare = match key.rfind('@') {
        Some(at) if at > 0 && (!scoped || key.find('/').is_some_and(|slash| at > slash)) => &key[..at],
        _ => key,
    };
    if scoped {
        return bare.to_owned();
    }
    if bare.contains(['/', '\\']) {
        return bare.rsplit(['/', '\\']).find(|part| !part.is_empty()).unwrap_or(bare).to_owned();
    }
    bare.to_owned()
}

fn read_database(value: &Value) -> Vec<DatabaseEntry> {
    match value {
        Value::Array(items) => items
            .iter()
            .filter_map(|item| {
                let object = item.as_object()?;
                let name = object.get("name")?.as_str()?;
                let marketplace = object.get("marketplace")?.as_str()?;
                object.get("installPath")?.as_str()?;
                database_entry(format!("{name}@{marketplace}"), item)
            })
            .collect(),
        Value::Object(root) => {
            let Some(plugins) = root.get("plugins").and_then(Value::as_object) else {
                return Vec::new();
            };
            let single_entry = root.get("version").and_then(Value::as_i64) == Some(1);
            plugins
                .iter()
                .filter_map(|(key, value)| {
                    let entry = if single_entry { value } else { value.as_array()?.first()? };
                    database_entry(key.clone(), entry)
                })
                .collect()
        }
        _ => Vec::new(),
    }
}

fn database_entry(key: String, value: &Value) -> Option<DatabaseEntry> {
    let object = value.as_object()?;
    let scope = match object.get("scope") {
        None | Some(Value::Null) => InstallScope::User,
        Some(Value::String(scope)) => match scope.as_str() {
            "user" => InstallScope::User,
            "project" => InstallScope::Project,
            "local" => InstallScope::Local,
            "managed" => InstallScope::Managed,
            _ => return None,
        },
        Some(_) => return None,
    };
    let install_path = object.get("installPath")?.as_str()?.to_owned();
    let optional_text = |field: &str| match object.get(field) {
        None | Some(Value::Null) => Some(None),
        Some(Value::String(text)) => Some(Some(text.clone())),
        Some(_) => None,
    };
    Some(DatabaseEntry {
        key,
        scope,
        install_path,
        version: optional_text("version")?,
        project_path: optional_text("projectPath")?,
    })
}

fn enabled_plugins(settings_path: &Path) -> Option<Map<String, Value>> {
    match read_json_file(settings_path) {
        JsonFile::Parsed(value) => value.get("enabledPlugins").and_then(Value::as_object).cloned(),
        JsonFile::Missing | JsonFile::Broken(_) => None,
    }
}

fn expand_home(path: &str, home: &Path) -> PathBuf {
    if path == "~" {
        return home.to_owned();
    }
    match path.strip_prefix("~/").or_else(|| path.strip_prefix("~\\")) {
        Some(rest) => home.join(rest),
        None => PathBuf::from(path),
    }
}

fn absolute_normalized(path: &Path) -> PathBuf {
    normalize_path(&std::path::absolute(path).unwrap_or_else(|_| path.to_owned()))
}

/// Whether `target` is `root` or below it, comparing whole path components.
pub fn path_contains(root: &Path, target: &Path) -> bool {
    absolute_normalized(target).starts_with(absolute_normalized(root))
}

fn scope_applies(entry: &DatabaseEntry, cwd: &Path, home: &Path) -> bool {
    match entry.scope {
        InstallScope::User | InstallScope::Managed => true,
        InstallScope::Project | InstallScope::Local => entry
            .project_path
            .as_deref()
            .filter(|path| !path.trim().is_empty())
            .is_some_and(|path| path_contains(&expand_home(path, home), cwd)),
    }
}

/// Leading `major.minor.patch` of a directory name and whether nothing follows it.
fn leading_version(name: &str) -> Option<(u64, u64, u64, bool)> {
    let mut rest = name;
    let mut parts = [0_u64; 3];
    for (index, part) in parts.iter_mut().enumerate() {
        if index > 0 {
            rest = rest.strip_prefix('.')?;
        }
        let digits = rest.len() - rest.trim_start_matches(|ch: char| ch.is_ascii_digit()).len();
        if digits == 0 {
            return None;
        }
        *part = rest[..digits].parse().ok()?;
        rest = &rest[digits..];
    }
    Some((parts[0], parts[1], parts[2], rest.is_empty()))
}

/// Preference among replacement install directories; the greatest wins. Remaining ties go to the
/// name that sorts first.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
struct CandidateRank {
    version: Option<(u64, u64, u64, bool)>,
    not_unknown: bool,
    name: std::cmp::Reverse<String>,
}

fn replacement_install_dir(configured: &Path, expected_name: &str) -> Option<PathBuf> {
    let parent = configured.parent().filter(|parent| !parent.as_os_str().is_empty())?;
    let mut best: Option<(CandidateRank, PathBuf)> = None;
    for child in std::fs::read_dir(parent).ok()?.flatten() {
        let path = child.path();
        if !path.is_dir() {
            continue;
        }
        let Ok(Some(manifest)) = read_manifest(&path) else { continue };
        if manifest.get("name").and_then(Value::as_str) != Some(expected_name) {
            continue;
        }
        let name = child.file_name().to_string_lossy().into_owned();
        let rank = CandidateRank { version: leading_version(&name), not_unknown: name != "unknown", name: std::cmp::Reverse(name) };
        if best.as_ref().is_none_or(|(current, _)| rank > *current) {
            best = Some((rank, path));
        }
    }
    best.map(|(_, path)| path)
}

/// Find the plugins Claude Code has installed that are enabled and apply to `cwd`.
pub fn discover_plugins(locations: &ClaudeLocations, cwd: &Path) -> PluginDiscovery {
    let mut discovery = PluginDiscovery::default();
    let database_path = locations.plugin_database_path();
    let entries = match read_json_file(&database_path) {
        JsonFile::Missing => return discovery,
        JsonFile::Broken(reason) => {
            discovery.log.push(format!("Claude Code plugin database {} ignored: {reason}", database_path.display()));
            return discovery;
        }
        JsonFile::Parsed(value) => read_database(&value),
    };
    let enabled = enabled_plugins(&locations.user_settings_path());
    for entry in entries {
        let is_enabled = enabled.as_ref().and_then(|map| map.get(&entry.key)).and_then(Value::as_bool).unwrap_or(true);
        if !is_enabled || !scope_applies(&entry, cwd, &locations.home) {
            continue;
        }
        let derived_name = derive_plugin_name(&entry.key);
        let configured = PathBuf::from(&entry.install_path);
        let install_dir = if configured.exists() {
            configured
        } else if let Some(replacement) = replacement_install_dir(&configured, &derived_name) {
            replacement
        } else {
            discovery.problems.push(PluginProblem {
                key: entry.key,
                install_path: entry.install_path,
                reason: "the install directory is missing and no installed version replaces it".to_owned(),
            });
            continue;
        };
        let install_dir = std::path::absolute(&install_dir).unwrap_or(install_dir);
        let manifest = match read_manifest(&install_dir) {
            Ok(manifest) => manifest,
            Err(reason) => {
                discovery.log.push(format!("Claude Code plugin {} manifest ignored: {reason}", entry.key));
                None
            }
        };
        let manifest_field = |field: &str| non_blank_text(manifest.as_ref().and_then(|manifest| manifest.get(field)));
        let name = manifest_field("name").map_or(derived_name, str::to_owned);
        let database_version = entry.version.as_deref().map(str::trim).unwrap_or("");
        let version = if !database_version.is_empty() && database_version != "unknown" {
            database_version.to_owned()
        } else if let Some(version) = manifest_field("version") {
            version.to_owned()
        } else if !database_version.is_empty() {
            database_version.to_owned()
        } else {
            "unknown".to_owned()
        };
        let hooks_path = Some(install_dir.join(PLUGIN_HOOKS_FILE)).filter(|path| path.is_file());
        discovery.plugins.push(DiscoveredPlugin {
            key: entry.key,
            name,
            version,
            scope: entry.scope,
            install_path: install_dir,
            manifest,
            hooks_path,
        });
    }
    discovery
}

/// Replace every plugin-root placeholder in every string value, at any depth.
pub fn substitute_plugin_root(value: &mut Value, root: &str) {
    match value {
        Value::String(text) if text.contains(PLUGIN_ROOT_PLACEHOLDER) => {
            *text = text.replace(PLUGIN_ROOT_PLACEHOLDER, root);
        }
        Value::Array(items) => items.iter_mut().for_each(|item| substitute_plugin_root(item, root)),
        Value::Object(map) => map.values_mut().for_each(|item| substitute_plugin_root(item, root)),
        _ => {}
    }
}

fn next_display_order(parsed: &ParsedHookConfig) -> usize {
    parsed
        .executable_handlers
        .iter()
        .map(|handler| &handler.source)
        .chain(parsed.diagnostics.iter().map(|diagnostic| &diagnostic.source))
        .map(|source| source.display_order + 1)
        .max()
        .unwrap_or(0)
}

fn pre_session_source(scope: HookSourceScope, path: &Path, display_order: usize) -> HookSourceMetadata {
    HookSourceMetadata {
        scope,
        source_path: path.to_string_lossy().into_owned(),
        display_order,
        discovered_at: HookDiscoveryTiming::PreSession,
        plugin_root: None,
        manifest_path: None,
        plugin_env: None,
    }
}

fn data_dir_name(key: &str) -> String {
    key.chars().map(|ch| if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') { ch } else { '-' }).collect()
}

/// Append Claude Code settings hooks (user, project, project local) and, with the
/// `claude-plugins` feature, installed-plugin hooks after the sources already in `parsed`.
pub fn append_claude_hooks(options: &HookConfigLoaderOptions, locations: &ClaudeLocations, parsed: &mut ParsedHookConfig) {
    let mut order = next_display_order(parsed);
    let cwd = Path::new(&options.cwd);
    let settings = [
        (locations.user_settings_path(), HookSourceScope::Global),
        (cwd.join(".claude").join("settings.json"), HookSourceScope::Project),
        (cwd.join(".claude").join("settings.local.json"), HookSourceScope::Project),
    ];
    for (path, scope) in settings {
        let config = match read_json_file(&path) {
            JsonFile::Missing => continue,
            JsonFile::Parsed(config) => config,
            JsonFile::Broken(reason) => {
                parsed.diagnostics.push(diagnostic(
                    DiagnosticDraft {
                        code: "invalid_root",
                        message: format!("Claude Code settings file {} was skipped: {reason}", path.display()),
                        path: "$".to_owned(),
                        event: None,
                        severity: None,
                    },
                    &pre_session_source(scope, &path, order),
                ));
                order += 1;
                continue;
            }
        };
        if config.get("hooks").is_none() {
            continue;
        }
        let loaded = parse_hook_config(&config, &pre_session_source(scope, &path, order));
        order += 1;
        parsed.executable_handlers.extend(loaded.executable_handlers);
        parsed.diagnostics.extend(loaded.diagnostics);
    }
    if cfg!(feature = "claude-plugins") {
        append_plugin_hooks(options, locations, cwd, order, parsed);
    }
}

fn append_plugin_hooks(options: &HookConfigLoaderOptions, locations: &ClaudeLocations, cwd: &Path, mut order: usize, parsed: &mut ParsedHookConfig) {
    let discovery = discover_plugins(locations, cwd);
    let database_path = locations.plugin_database_path();
    for problem in &discovery.problems {
        let mut source = pre_session_source(HookSourceScope::Plugin, &database_path, order);
        order += 1;
        source.plugin_root = Some(problem.install_path.clone());
        parsed.diagnostics.push(diagnostic(
            DiagnosticDraft {
                code: "invalid_plugin",
                message: format!("Claude Code plugin {} was skipped: {} (installPath {}).", problem.key, problem.reason, problem.install_path),
                path: format!("plugins.{}", problem.key),
                event: None,
                severity: None,
            },
            &source,
        ));
    }
    let mut log = discovery.log;
    let explicit = options
        .pre_session_hook_source_paths
        .iter()
        .chain(&options.runtime_hook_source_paths)
        .map(|path| absolute_normalized(Path::new(path)))
        .collect::<Vec<_>>();
    for plugin in discovery.plugins {
        let Some(hooks_path) = &plugin.hooks_path else { continue };
        if explicit.contains(&absolute_normalized(hooks_path)) {
            continue;
        }
        let mut config = match read_json_file(hooks_path) {
            JsonFile::Parsed(config) => config,
            JsonFile::Missing => continue,
            JsonFile::Broken(reason) => {
                log.push(format!("Claude Code plugin {} hooks ignored: {}: {reason}", plugin.key, hooks_path.display()));
                continue;
            }
        };
        let root = plugin.install_path.to_string_lossy().into_owned();
        substitute_plugin_root(&mut config, &root);
        let data_dir = locations.claude_dir.join("plugins").join("data").join(data_dir_name(&plugin.key));
        let plugin_env = match build_plugin_env(&plugin.install_path, Some(&data_dir)) {
            Ok(env) => Some(env),
            Err(error) => {
                log.push(format!("Claude Code plugin {} environment unavailable: {error}", plugin.key));
                None
            }
        };
        let mut source = pre_session_source(HookSourceScope::Plugin, hooks_path, order);
        source.plugin_root = Some(root);
        source.manifest_path = Some(plugin.install_path.join(PRIMARY_MANIFEST).to_string_lossy().into_owned());
        source.plugin_env = plugin_env;
        order += 1;
        let loaded = parse_hook_config(&config, &source);
        parsed.executable_handlers.extend(loaded.executable_handlers);
        parsed.diagnostics.extend(loaded.diagnostics);
    }
    for line in log {
        maho_ext_api::diagnostic(&line);
    }
}
