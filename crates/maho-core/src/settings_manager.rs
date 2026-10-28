//! Port of senpi packages/coding-agent/src/core/settings-manager.ts.
//!
//! Settings are an open-ended JSON object: the typed Settings interface in senpi is a runtime
//! record, so this port keeps a serde_json::Map (insertion order preserved) and exposes typed
//! accessors for the fields the engine reads and writes. Unknown keys survive every read/write, and
//! the JSONC parser matches senpi's (comments and trailing commas removed without touching
//! comment-like text inside strings).

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};

use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::config::config_dir_name;
use crate::lockfile_policy::{
    CredentialStoreBusyError, FILE_STORAGE_SYNC_LOCK_BUDGET_MS, acquire_lock_sync,
};
use crate::nearest_parent_config::find_nearest_parent_config_dir;
use crate::paths::{PathInputOptions, resolve_path};
use crate::settings_overrides::without_override;
use crate::text::strip_bom;

pub const DEFAULT_STREAM_START_TIMEOUT_MS: u64 = 300_000;
pub const DEFAULT_PROVIDER_STREAM_RETRY_TIMEOUT_MS: u64 = 30_000;
pub const DEFAULT_SESSION_SHUTDOWN_HANDLER_WARN_MS: u64 = 2_000;
pub const DEFAULT_SESSION_SHUTDOWN_HANDLER_TIMEOUT_MS: u64 = 10_000;
/// Total wait across every `session_shutdown` handler when the process quits (senpi#3711).
pub const DEFAULT_SESSION_SHUTDOWN_QUIT_BUDGET_MS: u64 = 2_000;
/// senpi `MAX_ENGINE_TURNS_PER_USER_INPUT`.
pub const MAX_ENGINE_TURNS_PER_USER_INPUT: i64 = 150;
/// senpi `MAX_ENGINE_TURNS_PER_RESUME_WINDOW`.
pub const MAX_ENGINE_TURNS_PER_RESUME_WINDOW: i64 = 8;
/// senpi `MAX_TOOL_FREE_ENGINE_TURNS_PER_WINDOW`.
pub const MAX_TOOL_FREE_ENGINE_TURNS_PER_WINDOW: i64 = 12;
pub const SELF_WRITE_TTL_MS: u64 = 15_000;
pub const MAX_SELF_WRITES_PER_PATH: usize = 8;

pub type Settings = Map<String, Value>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SettingsScope {
    Global,
    Project,
}

impl SettingsScope {
    pub fn as_str(self) -> &'static str {
        match self {
            SettingsScope::Global => "global",
            SettingsScope::Project => "project",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsFormat {
    Jsonc,
    Json,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsSourceReason {
    ExplicitJsonc,
    JsonOnly,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsSourceSelection {
    pub path: String,
    pub format: SettingsFormat,
    pub reason: SettingsSourceReason,
    pub scope: SettingsScope,
}

pub type SettingsSourceListener = Arc<dyn Fn(&SettingsSourceSelection) + Send + Sync>;
pub struct SettingsSourceSubscription {
    listeners: Arc<Mutex<Vec<SettingsSourceListener>>>,
    listener: SettingsSourceListener,
}
impl Drop for SettingsSourceSubscription {
    fn drop(&mut self) {
        self.listeners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|listener| !Arc::ptr_eq(listener, &self.listener));
    }
}

/// senpi `getProviderConcurrencyLimit` over a `providers` settings block.
pub fn provider_concurrency_limit(providers: Option<&Value>, provider_id: &str) -> Option<i64> {
    let providers: Option<std::collections::BTreeMap<String, Value>> =
        providers.and_then(Value::as_object).map(|providers| {
            providers
                .iter()
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect()
        });
    maho_ai::legacy_provider_ids::read_by_provider_id(providers.as_ref(), provider_id)
        .and_then(|settings| settings.get("maxConcurrency"))
        .and_then(Value::as_i64)
        .filter(|value| *value > 0)
}

/// senpi `subscribeToProviderSettings` listener: called after every settings recomputation.
pub type ProviderSettingsListener = Arc<dyn Fn(&SettingsManager) + Send + Sync>;
pub struct ProviderSettingsSubscription {
    listeners: Arc<Mutex<Vec<ProviderSettingsListener>>>,
    listener: ProviderSettingsListener,
}
impl Drop for ProviderSettingsSubscription {
    fn drop(&mut self) {
        self.listeners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|listener| !Arc::ptr_eq(listener, &self.listener));
    }
}

/// Parse JSON or JSONC without changing comment-like text inside strings.
pub fn parse_settings_json(content: &str) -> Result<Settings, String> {
    let content: Vec<char> = strip_bom(content).chars().collect();
    let mut without_comments: Vec<char> = Vec::with_capacity(content.len());
    let mut in_string = false;
    let mut escaped = false;
    let mut index = 0usize;

    while index < content.len() {
        let character = content[index];
        let next = content.get(index + 1).copied();
        if in_string {
            without_comments.push(character);
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                in_string = false;
            }
            index += 1;
            continue;
        }
        if character == '"' {
            in_string = true;
            without_comments.push(character);
            index += 1;
            continue;
        }
        if character == '/' && next == Some('/') {
            without_comments.push(' ');
            without_comments.push(' ');
            index += 2;
            while index < content.len() && content[index] != '\n' && content[index] != '\r' {
                without_comments.push(' ');
                index += 1;
            }
            if index < content.len() {
                without_comments.push(content[index]);
            }
            index += 1;
            continue;
        }
        if character == '/' && next == Some('*') {
            without_comments.push(' ');
            without_comments.push(' ');
            index += 2;
            let mut closed = false;
            while index < content.len() {
                if content[index] == '*' && content.get(index + 1) == Some(&'/') {
                    without_comments.push(' ');
                    without_comments.push(' ');
                    index += 2;
                    closed = true;
                    break;
                }
                without_comments.push(if content[index] == '\n' || content[index] == '\r' {
                    content[index]
                } else {
                    ' '
                });
                index += 1;
            }
            if !closed {
                return Err("Unterminated block comment in settings".to_owned());
            }
            continue;
        }
        without_comments.push(character);
        index += 1;
    }

    let mut normalized = without_comments;
    in_string = false;
    escaped = false;
    let mut index = 0usize;
    while index < normalized.len() {
        let character = normalized[index];
        if in_string {
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                in_string = false;
            }
            index += 1;
            continue;
        }
        if character == '"' {
            in_string = true;
            index += 1;
            continue;
        }
        if character != ',' {
            index += 1;
            continue;
        }
        let mut next_index = index + 1;
        while next_index < normalized.len() && normalized[next_index].is_whitespace() {
            next_index += 1;
        }
        if matches!(normalized.get(next_index), Some('}') | Some(']')) {
            normalized[index] = ' ';
        }
        index += 1;
    }

    let text: String = normalized.into_iter().collect();
    let parsed: Value = serde_json::from_str(&text).map_err(|error| error.to_string())?;
    match parsed {
        Value::Object(object) => Ok(object),
        _ => Err("Settings must contain a JSON object".to_owned()),
    }
}

struct SelfWriteTracker {
    writes_by_path: HashMap<String, Vec<(String, u64)>>,
}

fn self_writes() -> &'static Mutex<SelfWriteTracker> {
    static TRACKER: OnceLock<Mutex<SelfWriteTracker>> = OnceLock::new();
    TRACKER.get_or_init(|| {
        Mutex::new(SelfWriteTracker {
            writes_by_path: HashMap::new(),
        })
    })
}

type Clock = Box<dyn Fn() -> u64 + Send + Sync>;

fn self_write_clock() -> &'static Mutex<Clock> {
    static CLOCK: OnceLock<Mutex<Clock>> = OnceLock::new();
    CLOCK.get_or_init(|| Mutex::new(Box::new(default_clock)))
}

fn default_clock() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

fn now_ms() -> u64 {
    let clock = self_write_clock().lock().expect("self-write clock lock");
    clock()
}

pub fn record_self_write(abs_path: &str, content: &str) {
    let now = now_ms();
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    let hash = hex::encode(hasher.finalize());

    let mut tracker = self_writes().lock().expect("self-write lock");
    let writes = tracker
        .writes_by_path
        .entry(abs_path.to_owned())
        .or_default();
    writes.retain(|(_, recorded_at)| now.saturating_sub(*recorded_at) <= SELF_WRITE_TTL_MS);
    writes.retain(|(tracked_hash, _)| tracked_hash != &hash);
    writes.push((hash, now));
    while writes.len() > MAX_SELF_WRITES_PER_PATH {
        writes.remove(0);
    }
}

/// Whether a settings content hash was recently written by this process. A match is consumed.
pub fn was_self_write(abs_path: &str, hash: &str) -> bool {
    let now = now_ms();
    let mut tracker = self_writes().lock().expect("self-write lock");
    let Some(writes) = tracker.writes_by_path.get_mut(abs_path) else {
        return false;
    };
    writes.retain(|(_, recorded_at)| now.saturating_sub(*recorded_at) <= SELF_WRITE_TTL_MS);
    let before = writes.len();
    writes.retain(|(tracked_hash, _)| tracked_hash != hash);
    let consumed = writes.len() != before;
    if writes.is_empty() {
        tracker.writes_by_path.remove(abs_path);
    }
    consumed
}

pub fn reset_self_write_tracker_for_tests() {
    self_writes()
        .lock()
        .expect("self-write lock")
        .writes_by_path
        .clear();
}

pub fn set_self_write_tracker_clock_for_tests(clock: Option<Clock>) {
    let mut stored = self_write_clock().lock().expect("self-write clock lock");
    *stored = clock.unwrap_or_else(|| Box::new(default_clock));
}

pub fn get_settings_directory(
    cwd: &str,
    agent_dir: &str,
    scope: SettingsScope,
    home_dir: &str,
) -> String {
    if scope == SettingsScope::Global {
        return resolve_path(agent_dir, agent_dir, &PathInputOptions::default());
    }
    let resolved_cwd = resolve_path(cwd, cwd, &PathInputOptions::default());
    find_nearest_parent_config_dir(&resolved_cwd, home_dir, &config_dir_name(), None, None)
        .unwrap_or_else(|| {
            Path::new(&resolved_cwd)
                .join(config_dir_name())
                .to_string_lossy()
                .into_owned()
        })
}

/// Resolve the existing settings source, preferring JSONC when both formats exist.
pub fn resolve_settings_source(
    cwd: &str,
    agent_dir: &str,
    scope: SettingsScope,
    home_dir: &str,
) -> Option<SettingsSourceSelection> {
    let directory = get_settings_directory(cwd, agent_dir, scope, home_dir);
    let jsonc_path = Path::new(&directory)
        .join("settings.jsonc")
        .to_string_lossy()
        .into_owned();
    if Path::new(&jsonc_path).exists() {
        return Some(SettingsSourceSelection {
            path: jsonc_path,
            format: SettingsFormat::Jsonc,
            reason: SettingsSourceReason::ExplicitJsonc,
            scope,
        });
    }
    let json_path = Path::new(&directory)
        .join("settings.json")
        .to_string_lossy()
        .into_owned();
    if Path::new(&json_path).exists() {
        return Some(SettingsSourceSelection {
            path: json_path,
            format: SettingsFormat::Json,
            reason: SettingsSourceReason::JsonOnly,
            scope,
        });
    }
    None
}

pub fn get_settings_path(
    cwd: &str,
    agent_dir: &str,
    scope: SettingsScope,
    home_dir: &str,
) -> String {
    resolve_settings_source(cwd, agent_dir, scope, home_dir)
        .map(|source| source.path)
        .unwrap_or_else(|| {
            Path::new(&get_settings_directory(cwd, agent_dir, scope, home_dir))
                .join("settings.json")
                .to_string_lossy()
                .into_owned()
        })
}

pub fn get_in_memory_settings_path(scope: SettingsScope) -> String {
    match scope {
        SettingsScope::Global => "/__senpi_in_memory__/settings.json".to_owned(),
        SettingsScope::Project => "/__senpi_in_memory__/.senpi/settings.json".to_owned(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsError {
    pub scope: SettingsScope,
    pub path: Option<String>,
    pub error: String,
}

pub trait SettingsStorage: Send + Sync {
    /// Reads the current content, applies the update, and writes the result back under a lock.
    fn with_lock(
        &self,
        scope: SettingsScope,
        update: &mut dyn FnMut(Option<&str>) -> Option<String>,
    ) -> Result<(), String>;
    fn select_source(&self, _scope: SettingsScope) -> Option<SettingsSourceSelection> {
        None
    }
    /// Point a cwd-bound storage at another project directory; storage without a cwd ignores it.
    fn rebind_cwd(&mut self, _cwd: &str) {}
    /// The agent dir this storage was created for, when it has one.
    fn agent_dir(&self) -> Option<String> {
        None
    }
    /// senpi `withStateLock?`: the optional `state.json` bookkeeping store. Backends without one
    /// keep every key in the global settings file.
    fn has_state_store(&self) -> bool {
        false
    }
    fn with_state_lock(
        &self,
        _update: &mut dyn FnMut(Option<&str>) -> Option<String>,
    ) -> Option<Result<(), String>> {
        None
    }
}

pub struct FileSettingsStorage {
    cwd: String,
    agent_dir: String,
    home_dir: String,
    global_settings_path: Mutex<String>,
    project_settings_path: Mutex<String>,
}

impl FileSettingsStorage {
    pub fn new(cwd: &str, agent_dir: &str, home_dir: &str) -> Self {
        let global = get_settings_path(cwd, agent_dir, SettingsScope::Global, home_dir);
        let project = get_settings_path(cwd, agent_dir, SettingsScope::Project, home_dir);
        Self {
            cwd: cwd.to_owned(),
            agent_dir: agent_dir.to_owned(),
            home_dir: home_dir.to_owned(),
            global_settings_path: Mutex::new(global),
            project_settings_path: Mutex::new(project),
        }
    }

    pub fn settings_path(&self, scope: SettingsScope) -> String {
        let guard = match scope {
            SettingsScope::Global => self.global_settings_path.lock(),
            SettingsScope::Project => self.project_settings_path.lock(),
        };
        guard.expect("settings path lock").clone()
    }
}

impl SettingsStorage for FileSettingsStorage {
    fn agent_dir(&self) -> Option<String> {
        Some(self.agent_dir.clone())
    }

    fn rebind_cwd(&mut self, cwd: &str) {
        *self = Self::new(cwd, &self.agent_dir, &self.home_dir);
    }

    fn select_source(&self, scope: SettingsScope) -> Option<SettingsSourceSelection> {
        let source = resolve_settings_source(&self.cwd, &self.agent_dir, scope, &self.home_dir);
        let path = source
            .as_ref()
            .map(|source| source.path.clone())
            .unwrap_or_else(|| {
                Path::new(&get_settings_directory(
                    &self.cwd,
                    &self.agent_dir,
                    scope,
                    &self.home_dir,
                ))
                .join("settings.json")
                .to_string_lossy()
                .into_owned()
            });
        match scope {
            SettingsScope::Global => {
                *self
                    .global_settings_path
                    .lock()
                    .expect("settings path lock") = path
            }
            SettingsScope::Project => {
                *self
                    .project_settings_path
                    .lock()
                    .expect("settings path lock") = path
            }
        }
        source
    }

    fn with_lock(
        &self,
        scope: SettingsScope,
        update: &mut dyn FnMut(Option<&str>) -> Option<String>,
    ) -> Result<(), String> {
        with_path_lock(&self.settings_path(scope), update)
    }

    fn has_state_store(&self) -> bool {
        true
    }

    fn with_state_lock(
        &self,
        update: &mut dyn FnMut(Option<&str>) -> Option<String>,
    ) -> Option<Result<(), String>> {
        let agent_dir = resolve_path(
            &self.agent_dir,
            &self.agent_dir,
            &PathInputOptions::default(),
        );
        let path = Path::new(&agent_dir).join("state.json");
        Some(with_path_lock(&path.to_string_lossy(), update))
    }
}

fn with_path_lock(
    path: &str,
    update: &mut dyn FnMut(Option<&str>) -> Option<String>,
) -> Result<(), String> {
    let directory = Path::new(path)
        .parent()
        .map(|parent| parent.to_string_lossy().into_owned())
        .unwrap_or_default();

    // Read without the lock: writers publish atomically via temp+rename below, so a reader can
    // never observe partial content, and read-only callers skip lock acquisition entirely.
    let current = std::fs::read_to_string(path).ok();
    let mut next = update(current.as_deref());
    if next.is_none() {
        return Ok(());
    }
    if !Path::new(&directory).exists() {
        std::fs::create_dir_all(&directory)
            .map_err(|error| format!("Failed to write settings {path}: {error}"))?;
    }
    let mut guard = acquire_lock_sync(path, FILE_STORAGE_SYNC_LOCK_BUDGET_MS)
        .map_err(|error: CredentialStoreBusyError| error.message())?;
    let under_lock = std::fs::read_to_string(path).ok();
    if under_lock != current {
        // Lost a write race: re-merge against the winner's content under the lock.
        next = update(under_lock.as_deref());
    }
    if let Some(next) = next {
        // PERF-DIVERGENCE(settings-manager.ts): upstream rewrites unconditionally; this port skips the
        // write when content is byte-identical to disk, avoiding a self-induced hot reload. Output is identical.
        if under_lock.as_deref() == Some(next.as_str()) {
            return Ok(());
        }
        if guard.broker_write(&next)? {
            record_self_write(path, &next);
            return Ok(());
        }
        let temp_path = format!("{path}.{}.{}.tmp", std::process::id(), uuid::Uuid::new_v4());
        std::fs::write(&temp_path, &next).map_err(|error| {
            let _ = std::fs::remove_file(&temp_path);
            format!("Failed to write settings {path}: {error}")
        })?;
        guard.assert_held().map_err(|error| {
            let _ = std::fs::remove_file(&temp_path);
            format!("Failed to write settings {path}: lock lost before publishing ({error})")
        })?;
        record_self_write(path, &next);
        std::fs::rename(&temp_path, path).map_err(|error| {
            let _ = std::fs::remove_file(&temp_path);
            format!("Failed to write settings {path}: {error}")
        })?;
    }
    Ok(())
}

pub struct InMemorySettingsStorage {
    global: Mutex<Option<String>>,
    project: Mutex<Option<String>>,
    state: Mutex<Option<String>>,
}

impl Default for InMemorySettingsStorage {
    fn default() -> Self {
        Self {
            global: Mutex::new(None),
            project: Mutex::new(None),
            state: Mutex::new(None),
        }
    }
}

impl InMemorySettingsStorage {
    /// Seed the global settings document at construction, so a caller can install a real settings
    /// document (e.g. a retry/fallback chain) before building a `SettingsManager` over it.
    pub fn with_global_content(content: impl Into<String>) -> Self {
        Self {
            global: Mutex::new(Some(content.into())),
            project: Mutex::new(None),
            state: Mutex::new(None),
        }
    }

    pub fn content(&self, scope: SettingsScope) -> Option<String> {
        match scope {
            SettingsScope::Global => self.global.lock().expect("in-memory settings lock").clone(),
            SettingsScope::Project => self
                .project
                .lock()
                .expect("in-memory settings lock")
                .clone(),
        }
    }
}

impl SettingsStorage for InMemorySettingsStorage {
    fn with_lock(
        &self,
        scope: SettingsScope,
        update: &mut dyn FnMut(Option<&str>) -> Option<String>,
    ) -> Result<(), String> {
        let slot = match scope {
            SettingsScope::Global => &self.global,
            SettingsScope::Project => &self.project,
        };
        let mut guard = slot.lock().expect("in-memory settings lock");
        let next = update(guard.as_deref());
        if let Some(next) = next {
            record_self_write(&get_in_memory_settings_path(scope), &next);
            *guard = Some(next);
        }
        Ok(())
    }

    fn has_state_store(&self) -> bool {
        true
    }

    fn with_state_lock(
        &self,
        update: &mut dyn FnMut(Option<&str>) -> Option<String>,
    ) -> Option<Result<(), String>> {
        let mut guard = self.state.lock().expect("in-memory settings lock");
        if let Some(next) = update(guard.as_deref()) {
            *guard = Some(next);
        }
        Some(Ok(()))
    }
}

pub fn is_mergeable_object(value: &Value) -> bool {
    matches!(value, Value::Object(_))
}

pub fn deep_merge_objects(base: &Settings, overrides: &Settings) -> Settings {
    let mut result = base.clone();
    // senpi skips only `undefined`, which JSON cannot express: an explicit `null` replaces the base.
    for (key, override_value) in overrides {
        let merged = match (base.get(key), override_value) {
            (Some(Value::Object(base_object)), Value::Object(override_object)) => {
                Value::Object(deep_merge_objects(base_object, override_object))
            }
            _ => override_value.clone(),
        };
        result.insert(key.clone(), merged);
    }
    result
}

/// Deep merge settings: project/overrides take precedence, nested objects merge recursively.
/// `retry.fallbackChains` is the exception: an override's chain map replaces the base map wholesale.
pub fn deep_merge_settings(base: &Settings, overrides: &Settings) -> Settings {
    let mut result = deep_merge_objects(base, overrides);
    if let Some(chains) = overrides
        .get("retry")
        .and_then(|retry| retry.get("fallbackChains"))
        && let Some(Value::Object(retry)) = result.get_mut("retry")
    {
        retry.insert("fallbackChains".to_owned(), chains.clone());
    }
    result
}

/// senpi `ASK_USER_DEFAULT_TIMEOUT_MINUTES` / `ASK_USER_MIN_TIMEOUT_MINUTES` / `ASK_USER_MAX_TIMEOUT_MINUTES`.
pub const ASK_USER_DEFAULT_TIMEOUT_MINUTES: f64 = 30.0;
pub const ASK_USER_MIN_TIMEOUT_MINUTES: f64 = 1.0;
pub const ASK_USER_MAX_TIMEOUT_MINUTES: f64 = 120.0;

fn resolve_ask_user_timeout_minutes(value: Option<&Value>) -> f64 {
    match value
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite())
    {
        Some(value) => value
            .floor()
            .clamp(ASK_USER_MIN_TIMEOUT_MINUTES, ASK_USER_MAX_TIMEOUT_MINUTES),
        None => ASK_USER_DEFAULT_TIMEOUT_MINUTES,
    }
}

/// senpi `parseTimeoutSetting`: an unset value is `None`; a present value that is not a valid
/// timeout is an error naming the setting.
fn parse_timeout_setting(value: Option<&Value>, setting_name: &str) -> Result<Option<u64>, String> {
    let Some(value) = value else { return Ok(None) };
    if let Some(timeout_ms) = crate::http_dispatcher::parse_http_idle_timeout_ms(value) {
        return Ok(Some(timeout_ms));
    }
    let shown = value
        .as_str()
        .map_or_else(|| value.to_string(), str::to_owned);
    Err(format!("Invalid {setting_name} setting: {shown}"))
}

/// senpi `getEngineTurnSettings` return shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EngineTurnSettings {
    pub max_per_user_input: i64,
    pub max_per_resume_window: i64,
    pub max_tool_free_per_minute: i64,
}

/// senpi `getAskUserSettings` return shape.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AskUserSettings {
    pub enabled: bool,
    pub timeout_minutes: f64,
    pub bell: bool,
}

/// senpi `isTerminalMouseMode`.
pub fn is_terminal_mouse_mode(value: &str) -> bool {
    matches!(value, "off" | "whilePending" | "always")
}

/// senpi `envValue` over the process environment.
pub struct SettingsManager {
    storage: Box<dyn SettingsStorage>,
    global_settings: Settings,
    project_settings: Settings,
    settings: Settings,
    project_trusted: bool,
    global_settings_load_error: Option<String>,
    project_settings_load_error: Option<String>,
    errors: Vec<SettingsError>,
    settings_paths: HashMap<SettingsScope, String>,
    selected_sources: HashMap<SettingsScope, SettingsSourceSelection>,
    overrides: Settings,
    source_listeners: Arc<Mutex<Vec<SettingsSourceListener>>>,
    provider_settings_listeners: Arc<Mutex<Vec<ProviderSettingsListener>>>,
}

impl SettingsManager {
    pub fn from_storage(
        storage: Box<dyn SettingsStorage>,
        project_trusted: bool,
    ) -> SettingsManager {
        let mut settings_paths: HashMap<SettingsScope, String> = HashMap::new();
        let mut selected_sources: HashMap<SettingsScope, SettingsSourceSelection> = HashMap::new();
        if let Some(source) = storage.select_source(SettingsScope::Global) {
            settings_paths.insert(SettingsScope::Global, source.path.clone());
            selected_sources.insert(SettingsScope::Global, source);
        }
        let global_load =
            Self::try_load_from_storage(storage.as_ref(), SettingsScope::Global, true);
        if project_trusted && let Some(source) = storage.select_source(SettingsScope::Project) {
            settings_paths.insert(SettingsScope::Project, source.path.clone());
            selected_sources.insert(SettingsScope::Project, source);
        }
        let project_load =
            Self::try_load_from_storage(storage.as_ref(), SettingsScope::Project, project_trusted);

        let mut errors = Vec::new();
        if let Some(error) = &global_load.1 {
            errors.push(SettingsError {
                scope: SettingsScope::Global,
                path: settings_paths.get(&SettingsScope::Global).cloned(),
                error: error.clone(),
            });
        }
        if let Some(error) = &project_load.1 {
            errors.push(SettingsError {
                scope: SettingsScope::Project,
                path: settings_paths.get(&SettingsScope::Project).cloned(),
                error: error.clone(),
            });
        }

        let settings = deep_merge_settings(&global_load.0, &project_load.0);
        SettingsManager {
            storage,
            global_settings: global_load.0,
            project_settings: project_load.0,
            settings,
            project_trusted,
            global_settings_load_error: global_load.1,
            project_settings_load_error: project_load.1,
            errors,
            settings_paths,
            selected_sources,
            overrides: Settings::new(),
            source_listeners: Arc::default(),
            provider_settings_listeners: Arc::default(),
        }
    }

    pub fn create(
        cwd: &str,
        agent_dir: &str,
        home_dir: &str,
        project_trusted: bool,
    ) -> SettingsManager {
        let storage = FileSettingsStorage::new(cwd, agent_dir, home_dir);
        SettingsManager::from_storage(Box::new(storage), project_trusted)
    }

    /// Pinned `SettingsManager.inMemory(globalSettings, { projectTrusted })`: a manager backed by
    /// in-memory storage seeded with the given GLOBAL settings and no project scope.
    ///
    /// Startup theme resolution (`loadStartupThemes`) builds this over the session's LIVE global
    /// settings, so unsaved in-memory overrides are honoured instead of being lost to a fresh disk
    /// reload; project settings stay excluded (`projectTrusted: false`).
    pub fn in_memory(global_settings: Settings, project_trusted: bool) -> SettingsManager {
        let global_settings = migrate_settings(global_settings);
        let storage = InMemorySettingsStorage::default();
        // `with_lock` takes `&mut dyn FnMut` and may call the update closure more than once (a lost
        // write race re-runs it), so the closure must yield the seeded content on every call instead
        // of moving a captured `String` out of itself (E0507). Serialize per call from a borrow,
        // matching the pinned `inMemory` closure `() => JSON.stringify(initialSettings, null, 2)`.
        let _ = storage.with_lock(SettingsScope::Global, &mut |_| {
            serde_json::to_string(&global_settings).ok()
        });
        SettingsManager::from_storage(Box::new(storage), project_trusted)
    }

    fn try_load_from_storage(
        storage: &dyn SettingsStorage,
        scope: SettingsScope,
        trusted: bool,
    ) -> (Settings, Option<String>) {
        if !trusted {
            return (Settings::new(), None);
        }
        let mut loaded: Settings = Settings::new();
        let mut error: Option<String> = None;
        let mut apply = |current: Option<&str>| -> Option<String> {
            match current {
                None => {
                    loaded = Settings::new();
                }
                Some(content) => match parse_settings_json(content) {
                    Ok(parsed) => loaded = parsed,
                    Err(parse_error) => error = Some(parse_error),
                },
            }
            None
        };
        if let Err(storage_error) = storage.with_lock(scope, &mut apply)
            && error.is_none()
        {
            error = Some(storage_error);
        }
        if error.is_some() {
            return (Settings::new(), error);
        }
        if scope == SettingsScope::Global && storage.has_state_store() {
            if crate::settings_state::STATE_KEYS
                .iter()
                .any(|key| loaded.contains_key(*key))
            {
                crate::settings_state::migrate_settings_state(storage);
            }
            loaded.extend(crate::settings_state::read_settings_state(storage));
        }
        (migrate_settings(loaded), None)
    }

    pub fn get(&self) -> &Settings {
        &self.settings
    }

    pub fn get_global(&self) -> &Settings {
        &self.global_settings
    }

    pub fn get_project(&self) -> &Settings {
        &self.project_settings
    }

    pub fn is_project_trusted(&self) -> bool {
        self.project_trusted
    }

    /// senpi `getTodoFirstTurnPlan`: `remind` and `off` are honored; anything else is `force`.
    pub fn get_todo_first_turn_plan(&self) -> &'static str {
        match self.settings.get("todo").and_then(|todo| todo.get("firstTurnPlan")).and_then(Value::as_str) {
            Some("remind") => "remind",
            Some("off") => "off",
            _ => "force",
        }
    }

    /// senpi `getAskUserSettings`.
    pub fn get_ask_user_settings(&self) -> AskUserSettings {
        let configured = self.settings.get("askUser");
        let field = |key: &str| configured.and_then(|configured| configured.get(key));
        AskUserSettings {
            enabled: field("enabled").and_then(Value::as_bool).unwrap_or(true),
            timeout_minutes: resolve_ask_user_timeout_minutes(field("timeoutMinutes")),
            bell: field("bell").and_then(Value::as_bool).unwrap_or(true),
        }
    }

    /// senpi `setProjectTrusted`: an untrusted project drops its scope; a newly trusted project
    /// selects and loads its settings source.
    pub fn set_project_trusted(&mut self, trusted: bool) {
        if self.project_trusted == trusted {
            return;
        }
        self.project_trusted = trusted;
        if !trusted {
            self.project_settings = Settings::new();
            self.project_settings_load_error = None;
            self.settings = deep_merge_settings(
                &deep_merge_settings(&self.global_settings, &self.project_settings),
                &self.overrides,
            );
            return;
        }
        self.select_and_publish_source(SettingsScope::Project);
        let (project_settings, error) =
            Self::try_load_from_storage(self.storage.as_ref(), SettingsScope::Project, true);
        self.project_settings = project_settings;
        self.project_settings_load_error = error.clone();
        if let Some(error) = error {
            self.errors.push(SettingsError {
                scope: SettingsScope::Project,
                path: self.settings_paths.get(&SettingsScope::Project).cloned(),
                error,
            });
        }
        self.settings = deep_merge_settings(
            &deep_merge_settings(&self.global_settings, &self.project_settings),
            &self.overrides,
        );
    }

    pub fn errors(&self) -> &[SettingsError] {
        &self.errors
    }

    pub fn global_settings_load_error(&self) -> Option<&str> {
        self.global_settings_load_error.as_deref()
    }

    pub fn project_settings_load_error(&self) -> Option<&str> {
        self.project_settings_load_error.as_deref()
    }

    pub fn settings_path(&self, scope: SettingsScope) -> Option<&str> {
        self.settings_paths.get(&scope).map(String::as_str)
    }

    pub fn selected_source(&self, scope: SettingsScope) -> Option<&SettingsSourceSelection> {
        self.selected_sources.get(&scope)
    }

    pub fn get_selected_settings_sources(&self) -> Vec<SettingsSourceSelection> {
        self.selected_sources.values().cloned().collect()
    }

    pub fn canonical_global_settings_path(&self) -> Option<&str> {
        self.settings_paths
            .get(&SettingsScope::Global)
            .map(String::as_str)
    }

    pub fn subscribe_to_source_selection(
        &self,
        listener: SettingsSourceListener,
    ) -> SettingsSourceSubscription {
        self.source_listeners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(listener.clone());
        SettingsSourceSubscription {
            listeners: self.source_listeners.clone(),
            listener,
        }
    }

    /// senpi `getProviderConcurrencyLimit`: `providers.<id>.maxConcurrency` (canonical id first,
    /// then legacy spellings); `None` = unlimited.
    pub fn get_provider_concurrency_limit(&self, provider_id: &str) -> Option<i64> {
        provider_concurrency_limit(self.settings.get("providers"), provider_id)
    }

    /// The raw `providers` settings block, for callers that snapshot it (`ModelRuntime`).
    pub fn get_provider_settings(&self) -> Option<Value> {
        self.settings.get("providers").cloned()
    }

    pub fn subscribe_to_provider_settings(
        &self,
        listener: ProviderSettingsListener,
    ) -> ProviderSettingsSubscription {
        self.provider_settings_listeners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(listener.clone());
        ProviderSettingsSubscription {
            listeners: self.provider_settings_listeners.clone(),
            listener,
        }
    }

    /// senpi `updateSettings`: the merged settings changed, so provider-settings listeners re-read.
    fn notify_provider_settings(&self) {
        let listeners = self
            .provider_settings_listeners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        for listener in listeners {
            listener(self);
        }
    }

    fn select_and_publish_source(&mut self, scope: SettingsScope) {
        let Some(source) = self.storage.select_source(scope) else {
            self.selected_sources.remove(&scope);
            return;
        };
        self.settings_paths.insert(scope, source.path.clone());
        self.selected_sources.insert(scope, source.clone());
        let listeners = self
            .source_listeners
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        for listener in listeners {
            listener(&source);
        }
    }

    pub fn get_value(&self, key: &str) -> Option<&Value> {
        self.settings.get(key)
    }

    /// `getSessionShutdownQuitBudgetMs`: total wait across all `session_shutdown` handlers on quit; 0 disables.
    pub fn get_session_shutdown_quit_budget_ms(&self) -> Result<u64, String> {
        Ok(parse_timeout_setting(
            self.get_value("sessionShutdownQuitBudgetMs"),
            "sessionShutdownQuitBudgetMs",
        )?
        .unwrap_or(DEFAULT_SESSION_SHUTDOWN_QUIT_BUDGET_MS))
    }

    /// `getEngineTurnSettings`: non-negative integer overrides of the engine-turn bounds (senpi#2967).
    pub fn get_engine_turn_settings(&self) -> EngineTurnSettings {
        let configured = self.get_value("engineTurns");
        let limit = |key: &str, fallback: i64| -> i64 {
            configured
                .and_then(|turns| turns.get(key))
                .and_then(Value::as_i64)
                .filter(|value| *value >= 0)
                .unwrap_or(fallback)
        };
        EngineTurnSettings {
            max_per_user_input: limit("maxPerUserInput", MAX_ENGINE_TURNS_PER_USER_INPUT),
            max_per_resume_window: limit("maxPerResumeWindow", MAX_ENGINE_TURNS_PER_RESUME_WINDOW),
            max_tool_free_per_minute: limit(
                "maxToolFreePerMinute",
                MAX_TOOL_FREE_ENGINE_TURNS_PER_WINDOW,
            ),
        }
    }

    /// `getEnableSessionShare`: default true; false removes `/share` (global or project setting).
    pub fn get_enable_session_share(&self) -> bool {
        self.get_value("enableSessionShare")
            .and_then(Value::as_bool)
            .unwrap_or(true)
    }

    /// `handoffHint` settings with their defaults (senpi#3230).
    pub fn get_handoff_hint_settings(&self) -> crate::settings_shapes::ResolvedHandoffHintSettings {
        let configured = self
            .get_value("handoffHint")
            .cloned()
            .and_then(|value| {
                serde_json::from_value::<crate::settings_shapes::HandoffHintSettings>(value).ok()
            })
            .unwrap_or_default();
        configured.resolve()
    }

    pub fn get_model_service_tier(&self, provider: &str, model_id: &str) -> Option<&str> {
        let key = format!("{provider}/{model_id}");
        self.settings
            .get("modelServiceTiers")?
            .get(&key)?
            .as_str()
            .filter(|tier| matches!(*tier, "auto" | "flex" | "priority"))
    }

    pub fn set_model_service_tier(
        &mut self,
        provider: &str,
        model_id: &str,
        tier: &str,
    ) -> Result<(), String> {
        if !matches!(tier, "auto" | "flex" | "priority") {
            return Err(format!("Invalid model service tier: {tier}"));
        }
        let mut tiers = self
            .global_settings
            .get("modelServiceTiers")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        tiers.insert(
            format!("{provider}/{model_id}"),
            Value::String(tier.to_owned()),
        );
        self.set(
            SettingsScope::Global,
            &Settings::from_iter([("modelServiceTiers".into(), Value::Object(tiers))]),
        )
    }

    pub fn get_string(&self, key: &str) -> Option<String> {
        self.settings
            .get(key)
            .and_then(Value::as_str)
            .map(str::to_owned)
    }

    /// senpi `getExperimentalSharedHost`: `experimental.sharedHost === true`.
    pub fn get_experimental_shared_host(&self) -> bool {
        self.settings
            .get("experimental")
            .and_then(|experimental| experimental.get("sharedHost"))
            .and_then(Value::as_bool)
            == Some(true)
    }

    /// senpi `getShellPath`: a non-empty `shellPath` goes through `normalizePath` (`~` expansion,
    /// `file://` URLs, Windows shell paths).
    pub fn get_shell_path(&self) -> Option<String> {
        self.get_string("shellPath").map(|shell_path| {
            if shell_path.is_empty() {
                shell_path
            } else {
                crate::paths::normalize_path(&shell_path, &PathInputOptions::default())
            }
        })
    }

    pub fn get_bool(&self, key: &str) -> Option<bool> {
        self.settings.get(key).and_then(Value::as_bool)
    }

    /// senpi `getDisabledBuiltinExtensions`: builtin extension ids to skip loading.
    pub fn get_disabled_builtin_extensions(&self) -> Vec<String> {
        string_list(self.settings.get("disabledBuiltinExtensions")).unwrap_or_default()
    }

    /// `bashEnvPassthrough`: variable names kept in model-run command environments despite scrubbing.
    pub fn get_bash_env_passthrough(&self) -> Vec<String> {
        string_list(self.settings.get("bashEnvPassthrough")).unwrap_or_default()
    }

    /// `bashEnvScrub` (default true): scrub secret-looking variables from model-run commands.
    pub fn get_bash_env_scrub(&self) -> bool {
        self.get_bool("bashEnvScrub").unwrap_or(true)
    }

    /// The `BashToolOptions.env_passthrough` value: `Some(passthrough)` when scrubbing is enabled.
    pub fn get_bash_env_scrub_passthrough(&self) -> Option<Vec<String>> {
        self.get_bash_env_scrub().then(|| self.get_bash_env_passthrough())
    }

    /// The agent dir this manager was created for (the process default for storages without one).
    pub fn agent_dir(&self) -> String {
        self.storage.agent_dir().unwrap_or_else(crate::config::get_agent_dir)
    }

    /// `bashSandbox` (`off` default | `auto` | `required`): opt-in OS write confinement for bash.
    pub fn get_bash_sandbox(&self) -> maho_tools::bash_sandbox::BashSandboxMode {
        self.get_string("bashSandbox")
            .map_or(maho_tools::bash_sandbox::BashSandboxMode::Off, |value| {
                maho_tools::bash_sandbox::BashSandboxMode::parse(&value)
            })
    }

    /// senpi `getEnabledBuiltinExtensions`: optional allowlist of builtin extension ids (default: all).
    pub fn get_enabled_builtin_extensions(&self) -> Option<Vec<String>> {
        string_list(self.settings.get("enabledBuiltinExtensions"))
    }

    /// senpi `getTerminalCapabilityOverrides`: `terminal.images`/`trueColor`/`hyperlinks`, falling back
    /// to the brand-prefixed `IMAGE_PROTOCOL`/`TRUE_COLOR`/`HYPERLINKS` env values.
    pub fn get_terminal_capability_overrides(
        &self,
        env: &HashMap<String, String>,
    ) -> maho_tui::terminal_image::CapabilityOverrides {
        use maho_tui::terminal_capabilities::ImageProtocol;
        let terminal = self.settings.get("terminal");
        let setting = |key: &str| {
            terminal
                .and_then(|terminal| terminal.get(key))
                .filter(|value| !value.is_null())
                .cloned()
        };
        let env_value = |suffix: &str| crate::brand::env_value(suffix, env).map(Value::String);
        let images = match setting("images").or_else(|| {
            env_value("IMAGE_PROTOCOL")
                .map(|value| Value::String(value.as_str().unwrap_or_default().to_lowercase()))
        }) {
            Some(Value::String(value)) if value == "kitty" => Some(Some(ImageProtocol::Kitty)),
            Some(Value::String(value)) if value == "iterm2" => Some(Some(ImageProtocol::Iterm2)),
            Some(Value::Bool(false)) => Some(None),
            Some(Value::String(value)) if value == "none" || value == "0" => Some(None),
            _ => None,
        };
        let boolean = |key: &str, suffix: &str| match setting(key).or_else(|| env_value(suffix)) {
            Some(Value::Bool(value)) => Some(value),
            Some(Value::String(value)) if value == "1" || value == "0" => Some(value == "1"),
            _ => None,
        };
        maho_tui::terminal_image::CapabilityOverrides {
            images,
            true_color: boolean("trueColor", "TRUE_COLOR"),
            hyperlinks: boolean("hyperlinks", "HYPERLINKS"),
        }
    }

    fn terminal_setting(&self, key: &str) -> Option<&Value> {
        self.settings
            .get("terminal")
            .and_then(|terminal| terminal.get(key))
    }

    /// senpi `getShowImages`: `terminal.showImages`, default true.
    pub fn get_show_images(&self) -> bool {
        self.terminal_setting("showImages")
            .and_then(Value::as_bool)
            .unwrap_or(true)
    }

    /// senpi `setShowImages`.
    pub fn set_show_images(&mut self, show: bool) -> Result<(), String> {
        self.set_nested(
            SettingsScope::Global,
            "terminal",
            &Settings::from_iter([("showImages".into(), Value::Bool(show))]),
        )
    }

    /// senpi `getImageWidthCells`: `terminal.imageWidthCells` floored to at least 1, default 60.
    pub fn get_image_width_cells(&self) -> u32 {
        match self
            .terminal_setting("imageWidthCells")
            .and_then(Value::as_f64)
            .filter(|width| width.is_finite())
        {
            Some(width) => width.floor().max(1.0).min(f64::from(u32::MAX)) as u32,
            None => 60,
        }
    }

    /// senpi `setImageWidthCells`.
    pub fn set_image_width_cells(&mut self, width: u32) -> Result<(), String> {
        self.set_nested(
            SettingsScope::Global,
            "terminal",
            &Settings::from_iter([("imageWidthCells".into(), Value::from(width.max(1)))]),
        )
    }

    /// senpi `getTerminalMouse`: `terminal.mouse`, default `whilePending`.
    pub fn get_terminal_mouse(&self) -> String {
        self.terminal_setting("mouse")
            .and_then(Value::as_str)
            .filter(|value| is_terminal_mouse_mode(value))
            .unwrap_or("whilePending")
            .to_owned()
    }

    /// senpi `setTerminalMouse`.
    pub fn set_terminal_mouse(&mut self, mouse: &str) -> Result<(), String> {
        if !is_terminal_mouse_mode(mouse) {
            return Err("Invalid terminal.mouse".to_owned());
        }
        self.set_nested(
            SettingsScope::Global,
            "terminal",
            &Settings::from_iter([("mouse".into(), Value::String(mouse.to_owned()))]),
        )
    }

    /// senpi `getClearOnShrink`: the setting, then the brand `CLEAR_ON_SHRINK` env value, then false.
    pub fn get_clear_on_shrink(&self) -> bool {
        self.get_clear_on_shrink_with_env(&std::env::vars().collect())
    }

    /// `get_clear_on_shrink` against an explicit environment.
    pub fn get_clear_on_shrink_with_env(&self, env: &HashMap<String, String>) -> bool {
        if let Some(enabled) = self
            .terminal_setting("clearOnShrink")
            .and_then(Value::as_bool)
        {
            return enabled;
        }
        crate::brand::env_value("CLEAR_ON_SHRINK", env).as_deref() == Some("1")
    }

    /// senpi `setClearOnShrink`.
    pub fn set_clear_on_shrink(&mut self, enabled: bool) -> Result<(), String> {
        self.set_nested(
            SettingsScope::Global,
            "terminal",
            &Settings::from_iter([("clearOnShrink".into(), Value::Bool(enabled))]),
        )
    }

    /// senpi `getShowTerminalProgress`: `terminal.showTerminalProgress`, default false.
    pub fn get_show_terminal_progress(&self) -> bool {
        self.terminal_setting("showTerminalProgress")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }

    /// senpi `setShowTerminalProgress`.
    pub fn set_show_terminal_progress(&mut self, enabled: bool) -> Result<(), String> {
        self.set_nested(
            SettingsScope::Global,
            "terminal",
            &Settings::from_iter([("showTerminalProgress".into(), Value::Bool(enabled))]),
        )
    }

    pub fn get_image_auto_resize(&self) -> bool {
        self.settings
            .get("images")
            .and_then(|images| images.get("autoResize"))
            .and_then(Value::as_bool)
            .unwrap_or(true)
    }

    /// senpi `getCodeBlockIndent`: `markdown.codeBlockIndent`, defaulting to two spaces.
    pub fn get_code_block_indent(&self) -> String {
        self.settings
            .get("markdown")
            .and_then(|markdown| markdown.get("codeBlockIndent"))
            .and_then(Value::as_str)
            .unwrap_or("  ")
            .to_owned()
    }

    /// senpi `setImageAutoResize`.
    pub fn set_image_auto_resize(&mut self, enabled: bool) -> Result<(), String> {
        self.set_nested(
            SettingsScope::Global,
            "images",
            &Settings::from_iter([("autoResize".into(), Value::Bool(enabled))]),
        )
    }

    /// senpi `getMermaidRenderingMode`: `markdown.mermaid`, with anything but `off`/`final` read as
    /// `streaming`.
    pub fn get_mermaid_rendering_mode(&self) -> crate::settings_shapes::MermaidRenderingMode {
        use crate::settings_shapes::MermaidRenderingMode;
        match self
            .settings
            .get("markdown")
            .and_then(|markdown| markdown.get("mermaid"))
            .and_then(Value::as_str)
        {
            Some("off") => MermaidRenderingMode::Off,
            Some("final") => MermaidRenderingMode::Final,
            _ => MermaidRenderingMode::Streaming,
        }
    }

    /// senpi `setMermaidRenderingMode`: writes global `markdown.mermaid`, keeping the other
    /// `markdown` keys.
    pub fn set_mermaid_rendering_mode(
        &mut self,
        mode: crate::settings_shapes::MermaidRenderingMode,
    ) -> Result<(), String> {
        let mut markdown = self
            .global_settings
            .get("markdown")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        markdown.insert("mermaid".into(), Value::String(mode.as_str().to_owned()));
        self.set(
            SettingsScope::Global,
            &Settings::from_iter([("markdown".into(), Value::Object(markdown))]),
        )
    }

    pub fn get_block_images(&self) -> bool {
        self.settings
            .get("images")
            .and_then(|images| images.get("blockImages"))
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }

    /// senpi `setBlockImages`.
    pub fn set_block_images(&mut self, blocked: bool) -> Result<(), String> {
        self.set_nested(
            SettingsScope::Global,
            "images",
            &Settings::from_iter([("blockImages".into(), Value::Bool(blocked))]),
        )
    }

    /// senpi `setCompactionEnabled`: writes only `compaction.enabled`.
    pub fn set_compaction_enabled(&mut self, enabled: bool) -> Result<(), String> {
        self.set_nested(
            SettingsScope::Global,
            "compaction",
            &Settings::from_iter([("enabled".into(), Value::Bool(enabled))]),
        )
    }

    /// senpi `getShowHardwareCursor`: the setting, then the brand `HARDWARE_CURSOR` env value.
    pub fn get_show_hardware_cursor(&self) -> bool {
        self.get_show_hardware_cursor_with_env(&std::env::vars().collect())
    }

    /// `get_show_hardware_cursor` against an explicit environment.
    pub fn get_show_hardware_cursor_with_env(&self, env: &HashMap<String, String>) -> bool {
        self.settings
            .get("showHardwareCursor")
            .and_then(Value::as_bool)
            .unwrap_or_else(|| {
                crate::brand::env_value("HARDWARE_CURSOR", env).as_deref() == Some("1")
            })
    }

    /// senpi `getAutocompleteMaxVisible`: default 5.
    pub fn get_autocomplete_max_visible(&self) -> usize {
        self.settings
            .get("autocompleteMaxVisible")
            .and_then(Value::as_f64)
            .filter(|value| value.is_finite() && *value >= 0.0)
            .map_or(5, |value| value as usize)
    }

    /// senpi accepts only a non-negative integer (`Number.isInteger(value) && value >= 0`).
    pub fn get_max_historical_images(&self) -> Option<usize> {
        let value = self.settings.get("images")?.get("maxHistoricalImages")?;
        value
            .as_u64()
            .or_else(|| {
                value
                    .as_f64()
                    .filter(|number| *number >= 0.0 && number.fract() == 0.0)
                    .map(|number| number as u64)
            })
            .and_then(|number| usize::try_from(number).ok())
    }

    pub fn get_number(&self, key: &str) -> Option<f64> {
        self.settings.get(key).and_then(Value::as_f64)
    }

    pub fn get_raw_fallback_chains(&self) -> Option<&Value> {
        self.settings
            .get("retry")
            .and_then(|retry| retry.get("fallbackChains"))
    }

    /// senpi `setFallbackChain`: replaces one key of the GLOBAL chain map.
    pub fn set_fallback_chain(&mut self, key: &str, entries: &[String]) -> Result<(), String> {
        let mut chains = self.get_global_fallback_chains();
        chains.insert(
            key.to_owned(),
            Value::Array(entries.iter().cloned().map(Value::String).collect()),
        );
        self.set_nested(
            SettingsScope::Global,
            "retry",
            &Settings::from_iter([("fallbackChains".into(), Value::Object(chains))]),
        )
    }

    /// senpi `removeFallbackChain`.
    pub fn remove_fallback_chain(&mut self, key: &str) -> Result<(), String> {
        let mut chains = self.get_global_fallback_chains();
        if chains.remove(key).is_none() {
            return Ok(());
        }
        self.set_nested(
            SettingsScope::Global,
            "retry",
            &Settings::from_iter([("fallbackChains".into(), Value::Object(chains))]),
        )
    }

    /// senpi `setModelFallbackEnabled`.
    pub fn set_model_fallback_enabled(&mut self, enabled: bool) -> Result<(), String> {
        self.set_nested(
            SettingsScope::Global,
            "retry",
            &Settings::from_iter([("modelFallback".into(), Value::Bool(enabled))]),
        )
    }

    /// senpi `setFallbackRevertPolicy`: `cooldown-expiry` or `never`.
    pub fn set_fallback_revert_policy(&mut self, policy: &str) -> Result<(), String> {
        self.set_nested(
            SettingsScope::Global,
            "retry",
            &Settings::from_iter([(
                "fallbackRevertPolicy".into(),
                Value::String(policy.to_owned()),
            )]),
        )
    }

    /// senpi `getGlobalFallbackChains`: only well-formed string-array chains of the global scope.
    fn get_global_fallback_chains(&self) -> Map<String, Value> {
        let Some(Value::Object(chains)) = self
            .global_settings
            .get("retry")
            .and_then(|retry| retry.get("fallbackChains"))
        else {
            return Map::new();
        };
        chains
            .iter()
            .filter(|(_, entries)| {
                entries
                    .as_array()
                    .is_some_and(|entries| entries.iter().all(Value::is_string))
            })
            .map(|(key, entries)| (key.clone(), entries.clone()))
            .collect()
    }

    pub fn get_fallback_chains_scope(&self) -> Option<SettingsScope> {
        if self
            .project_settings
            .get("retry")
            .and_then(|retry| retry.get("fallbackChains"))
            .is_some()
        {
            return Some(SettingsScope::Project);
        }
        if self
            .global_settings
            .get("retry")
            .and_then(|retry| retry.get("fallbackChains"))
            .is_some()
        {
            return Some(SettingsScope::Global);
        }
        None
    }

    pub fn apply_overrides(&mut self, overrides: &Settings) {
        self.overrides = deep_merge_settings(&self.overrides, overrides);
        self.settings = deep_merge_settings(
            &deep_merge_settings(&self.global_settings, &self.project_settings),
            &self.overrides,
        );
        self.notify_provider_settings();
    }

    /// `getSteeringMode`: defaults to `all`.
    pub fn get_steering_mode(&self) -> maho_agent::types::QueueMode {
        match self.get_value("steeringMode").and_then(Value::as_str) {
            Some("one-at-a-time") => maho_agent::types::QueueMode::OneAtATime,
            _ => maho_agent::types::QueueMode::All,
        }
    }

    /// senpi `getTreeFilterMode`: one of the five tree filters, else `default`.
    pub fn get_tree_filter_mode(&self) -> String {
        self.get_value("treeFilterMode")
            .and_then(Value::as_str)
            .filter(|mode| {
                matches!(
                    *mode,
                    "default" | "no-tools" | "user-only" | "labeled-only" | "all"
                )
            })
            .unwrap_or("default")
            .to_owned()
    }

    /// senpi `getOutputPad`: `0` only when explicitly `0`, otherwise `1`.
    pub fn get_output_pad(&self) -> u8 {
        u8::from(self.get_value("outputPad").and_then(Value::as_f64) != Some(0.0))
    }

    /// `getFollowUpMode`: defaults to `one-at-a-time`.
    pub fn get_follow_up_mode(&self) -> maho_agent::types::QueueMode {
        match self.get_value("followUpMode").and_then(Value::as_str) {
            Some("all") => maho_agent::types::QueueMode::All,
            _ => maho_agent::types::QueueMode::OneAtATime,
        }
    }

    /// `getTransport`: defaults to `auto`.
    pub fn get_transport(&self) -> maho_ai::types::Transport {
        self.get_value("transport")
            .cloned()
            .and_then(|value| serde_json::from_value(value).ok())
            .unwrap_or(maho_ai::types::Transport::Auto)
    }

    /// `getAbortServerSideFallback`: defaults to enabled.
    pub fn get_abort_server_side_fallback(&self) -> bool {
        crate::retry_fallback::settings::resolve_abort_server_side_fallback(self.get_value("retry"))
    }

    /// `getHttpIdleTimeoutMs`: an invalid value is an error (`Invalid httpIdleTimeoutMs setting: <v>`).
    pub fn get_http_idle_timeout_ms(&self) -> Result<u64, String> {
        Ok(
            parse_timeout_setting(self.get_value("httpIdleTimeoutMs"), "httpIdleTimeoutMs")?
                .unwrap_or(crate::http_dispatcher::DEFAULT_HTTP_IDLE_TIMEOUT_MS),
        )
    }

    /// `setSessionShutdownQuitBudgetMs`: global scope; floors the value.
    pub fn set_session_shutdown_quit_budget_ms(&mut self, budget_ms: f64) -> Result<(), String> {
        if !budget_ms.is_finite() || budget_ms < 0.0 {
            return Err(format!(
                "Invalid sessionShutdownQuitBudgetMs setting: {budget_ms}"
            ));
        }
        self.set(
            SettingsScope::Global,
            &Settings::from_iter([(
                "sessionShutdownQuitBudgetMs".into(),
                Value::from(budget_ms.floor() as u64),
            )]),
        )
    }

    /// `getRetrySettings`. The `maxRetries` default is derived from the shipped profile so the one
    /// `retry.maxRetries` key means the same budget on every consumer.
    pub fn get_retry_settings(&self) -> maho_ai::utils::retry::RetryPolicy {
        let retry = self.get_value("retry");
        let field = |key: &str| retry.and_then(|retry| retry.get(key));
        maho_ai::utils::retry::RetryPolicy {
            enabled: field("enabled").and_then(Value::as_bool).unwrap_or(true),
            max_retries: field("maxRetries")
                .and_then(Value::as_u64)
                .and_then(|value| u32::try_from(value).ok())
                .unwrap_or(
                    maho_ai::utils::retry_profile::profiles::SENPI_DEFAULT_RETRY_PROFILE
                        .turn
                        .max_retries,
                ),
            base_delay_ms: field("baseDelayMs")
                .and_then(Value::as_u64)
                .unwrap_or(2_000),
            max_agent_delay_ms: Some(
                field("maxAgentDelayMs")
                    .and_then(Value::as_u64)
                    .unwrap_or(maho_ai::utils::retry::DEFAULT_MAX_AGENT_RETRY_DELAY_MS),
            ),
            random: None,
        }
    }

    /// `getProviderRetrySettings`.
    pub fn get_provider_retry_settings(&self) -> ProviderRequestRetrySettings {
        let provider = self
            .get_value("retry")
            .and_then(|retry| retry.get("provider"));
        let field = |key: &str| {
            provider
                .and_then(|provider| provider.get(key))
                .and_then(settings_ms)
        };
        ProviderRequestRetrySettings {
            timeout_ms: field("timeoutMs"),
            max_retries: field("maxRetries").and_then(|value| u32::try_from(value).ok()),
            max_retry_delay_ms: field("maxRetryDelayMs").unwrap_or(60_000),
        }
    }

    pub fn resolve_retry_profile(
        &self,
        provider: Option<&dyn maho_ai::models::Provider>,
    ) -> maho_ai::utils::retry_profile::types::RetryPolicyProfile {
        let declared = provider.and_then(maho_ai::models::Provider::retry_policy);
        let mut profile = declared.cloned().unwrap_or_else(|| {
            maho_ai::utils::retry_profile::profiles::SENPI_DEFAULT_RETRY_PROFILE.clone()
        });
        let retry = self.get_value("retry");
        if declared.is_none() {
            if let Some(max_retries) = retry
                .and_then(|value| value.get("maxRetries"))
                .and_then(Value::as_u64)
                .and_then(|value| u32::try_from(value).ok())
            {
                profile.turn.max_retries = max_retries;
            }
            if let Some(base_delay) = retry
                .and_then(|value| value.get("baseDelayMs"))
                .and_then(Value::as_f64)
            {
                profile.turn.backoff.base_delay_ms = base_delay;
            }
        }
        let validated =
            crate::retry_fallback::profile_override::validate_retry_provider_overrides(
                retry.and_then(|value| value.get("providers")),
                &provider
                    .map(|provider| std::collections::HashSet::from([provider.id().to_owned()]))
                    .unwrap_or_default(),
                Some(
                    &provider
                        .filter(|_| {
                            matches!(profile.turn.server_hint,
                maho_ai::utils::retry_profile::types::RetryServerHintPolicy::Tiered { .. })
                        })
                        .map(|provider| std::collections::HashSet::from([provider.id().to_owned()]))
                        .unwrap_or_default(),
                ),
            );
        let overrides =
            provider.and_then(|provider| validated.overrides.get(provider.id())?.get("turn"));
        if let Some(max_retries) = overrides
            .and_then(|value| value.get("maxRetries"))
            .and_then(Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
        {
            profile.turn.max_retries = max_retries;
        }
        if let Some(base_delay) = overrides
            .and_then(|value| value.get("baseDelayMs"))
            .and_then(Value::as_f64)
        {
            profile.turn.backoff.base_delay_ms = base_delay;
        }
        if let Some(growth) = overrides
            .and_then(|value| value.get("growthFactor"))
            .and_then(Value::as_f64)
        {
            profile.turn.backoff.growth_factor = growth;
        }
        if let Some(cap) = overrides.and_then(|value| value.get("perAttemptCapMs")) {
            profile.turn.backoff.per_attempt_cap_ms = cap.as_f64();
        }
        if let Some(jitter) = overrides.and_then(|value| value.get("jitter")) {
            use maho_ai::utils::retry_profile::types::RetryJitterPolicy;
            profile.turn.backoff.jitter = match jitter.get("mode").and_then(Value::as_str) {
                Some("additive") => RetryJitterPolicy::Additive {
                    ratio: jitter["ratio"].as_f64().expect("validated ratio"),
                },
                Some("subtractive") => RetryJitterPolicy::Subtractive {
                    ratio: jitter["ratio"].as_f64().expect("validated ratio"),
                },
                _ => RetryJitterPolicy::None,
            };
        }
        if let Some(cap) = overrides.and_then(|value| value.get("serverHintMaxDelayMs"))
            && let maho_ai::utils::retry_profile::types::RetryServerHintPolicy::Override {
                ceiling,
                ..
            } = &mut profile.turn.server_hint
        {
            ceiling.max_delay_ms = cap.as_u64();
        }
        profile.turn.enabled = retry
            .and_then(|value| value.get("enabled"))
            .and_then(Value::as_bool)
            .unwrap_or(true)
            && overrides
                .and_then(|value| value.get("enabled"))
                .and_then(Value::as_bool)
                .unwrap_or(profile.turn.enabled);
        profile
    }

    /// `getAgentStreamIdleTimeoutMs`: `retry.provider.timeoutMs` wins; otherwise `httpIdleTimeoutMs`,
    /// where 0 ("disabled") disables the reader guard as well.
    pub fn get_agent_stream_idle_timeout_ms(&self) -> Result<Option<u64>, String> {
        if let Some(provider_timeout_ms) = self.get_provider_retry_settings().timeout_ms {
            return Ok(Some(provider_timeout_ms));
        }
        let http_idle_timeout_ms = self.get_http_idle_timeout_ms()?;
        Ok((http_idle_timeout_ms != 0).then_some(http_idle_timeout_ms))
    }

    /// `getAgentStreamStartTimeoutMs`: `retry.provider.streamStartTimeoutMs` (0 disables), else the
    /// 300s default capped by the idle timeout and disabled together with it.
    pub fn get_agent_stream_start_timeout_ms(&self) -> Result<Option<u64>, String> {
        let explicit = self
            .get_value("retry")
            .and_then(|retry| retry.get("provider"))
            .and_then(|provider| provider.get("streamStartTimeoutMs"))
            .and_then(settings_ms);
        if let Some(explicit) = explicit {
            return Ok((explicit > 0).then_some(explicit));
        }
        Ok(self
            .get_agent_stream_idle_timeout_ms()?
            .map(|idle| DEFAULT_STREAM_START_TIMEOUT_MS.min(idle)))
    }

    /// `getWebSocketConnectTimeoutMs`: an invalid value is an error
    /// (`Invalid websocketConnectTimeoutMs setting: <v>`).
    pub fn get_websocket_connect_timeout_ms(&self) -> Result<Option<u64>, String> {
        parse_timeout_setting(
            self.get_value("websocketConnectTimeoutMs"),
            "websocketConnectTimeoutMs",
        )
    }

    /// `getThinkingBudgets`.
    pub fn get_thinking_budgets(&self) -> Option<maho_ai::types::ThinkingBudgets> {
        self.get_value("thinkingBudgets")
            .cloned()
            .and_then(|value| serde_json::from_value(value).ok())
    }

    /// senpi `markModified(field, nestedKey)` + `persistScopedSettings` for nested fields: writes only
    /// the given keys of `field`, merged into the scope file's current object, so sibling keys
    /// (on disk and in memory) survive. A `null` value removes that nested key. The written keys stop
    /// being overridden for this session.
    pub fn set_nested(
        &mut self,
        scope: SettingsScope,
        field: &str,
        nested: &Settings,
    ) -> Result<(), String> {
        if self.writes_state(scope, field) {
            self.persist_state(&|state| {
                let object = nested_into(state.get(field), nested);
                state.insert(field.to_owned(), Value::Object(object));
            });
        } else {
            let mut write_error: Option<String> = None;
            let mut apply = |current: Option<&str>| -> Option<String> {
                let mut parsed = match parse_current_settings(current) {
                    Ok(parsed) => parsed,
                    Err(error) => {
                        write_error = Some(error);
                        return None;
                    }
                };
                let object = nested_into(parsed.get(field), nested);
                parsed.insert(field.to_owned(), Value::Object(object));
                Some(serialize_settings(&parsed))
            };
            self.storage.with_lock(scope, &mut apply)?;
            if let Some(error) = write_error {
                return Err(error);
            }
        }
        for key in nested.keys() {
            self.overrides = without_override(&self.overrides, field, Some(key));
        }
        let target = match scope {
            SettingsScope::Global => &mut self.global_settings,
            SettingsScope::Project => &mut self.project_settings,
        };
        let object = nested_into(target.get(field), nested);
        target.insert(field.to_owned(), Value::Object(object));
        self.settings = deep_merge_settings(
            &deep_merge_settings(&self.global_settings, &self.project_settings),
            &self.overrides,
        );
        self.notify_provider_settings();
        Ok(())
    }

    /// Writes values into the given scope and recomputes the merged view. Each written field stops
    /// being overridden for this session (senpi `markModified` -> `withoutOverride`).
    pub fn set(&mut self, scope: SettingsScope, values: &Settings) -> Result<(), String> {
        let values = values.clone();
        let (state_values, config_values): (Settings, Settings) = values
            .clone()
            .into_iter()
            .partition(|(key, _)| self.writes_state(scope, key));
        if !state_values.is_empty() {
            self.persist_state(&|state| {
                *state = values_into(state, &state_values);
            });
        }
        if !config_values.is_empty() {
            let mut write_error: Option<String> = None;
            let mut apply = |current: Option<&str>| -> Option<String> {
                let parsed = match parse_current_settings(current) {
                    Ok(parsed) => parsed,
                    Err(error) => {
                        write_error = Some(error);
                        return None;
                    }
                };
                Some(serialize_settings(&values_into(&parsed, &config_values)))
            };
            self.storage.with_lock(scope, &mut apply)?;
            if let Some(error) = write_error {
                return Err(error);
            }
        }
        for key in values.keys() {
            self.overrides = without_override(&self.overrides, key, None);
        }
        match scope {
            SettingsScope::Global => {
                self.global_settings = values_into(&self.global_settings, &values);
                self.settings = deep_merge_settings(
                    &deep_merge_settings(&self.global_settings, &self.project_settings),
                    &self.overrides,
                );
                self.notify_provider_settings();
            }
            SettingsScope::Project => {
                self.project_settings = values_into(&self.project_settings, &values);
                self.settings = deep_merge_settings(
                    &deep_merge_settings(&self.global_settings, &self.project_settings),
                    &self.overrides,
                );
                self.notify_provider_settings();
            }
        }
        Ok(())
    }

    /// senpi `save`: bookkeeping keys of the global scope go to `state.json` when the storage has one.
    fn writes_state(&self, scope: SettingsScope, key: &str) -> bool {
        scope == SettingsScope::Global
            && self.storage.has_state_store()
            && crate::settings_state::is_state_key(key)
    }

    /// senpi `persistScopedSettings(..., state = true)`: a failed bookkeeping write is logged, never
    /// surfaced, so it cannot block the config write.
    fn persist_state(&self, mutate: &dyn Fn(&mut Settings)) {
        let storage = self.storage.as_ref();
        let result = storage.with_state_lock(&mut |current| {
            let current = migrate_settings(crate::settings_state::parse_state_settings(
                storage, current,
            ));
            let mut merged = current.clone();
            mutate(&mut merged);
            (merged != current).then(|| serialize_settings(&merged))
        });
        if let Some(Err(error)) = result {
            crate::settings_state::log_state_error(storage, &error);
        }
    }

    /// senpi recreates settings for a switched session's cwd; the native manager keeps its
    /// identity (listeners, overrides) and re-reads the project scope from the new cwd.
    pub fn rebind_cwd(&mut self, cwd: &str) {
        self.storage.rebind_cwd(cwd);
        self.settings_paths.remove(&SettingsScope::Project);
        self.selected_sources.remove(&SettingsScope::Project);
        self.reload();
    }

    pub fn reload(&mut self) {
        self.select_and_publish_source(SettingsScope::Global);
        let global_load =
            Self::try_load_from_storage(self.storage.as_ref(), SettingsScope::Global, true);
        if self.project_trusted {
            self.select_and_publish_source(SettingsScope::Project);
        }
        let project_load = Self::try_load_from_storage(
            self.storage.as_ref(),
            SettingsScope::Project,
            self.project_trusted,
        );
        self.global_settings = global_load.0;
        self.project_settings = project_load.0;
        self.global_settings_load_error = global_load.1;
        self.project_settings_load_error = project_load.1;
        self.settings = deep_merge_settings(
            &deep_merge_settings(&self.global_settings, &self.project_settings),
            &self.overrides,
        );
        self.notify_provider_settings();
    }
}

/// senpi `SettingsManager.migrateSettings`: rewrite old settings shapes to the current ones.
pub fn migrate_settings(mut settings: Settings) -> Settings {
    // queueMode -> steeringMode
    if settings.contains_key("queueMode")
        && !settings.contains_key("steeringMode")
        && let Some(queue_mode) = settings.remove("queueMode")
    {
        settings.insert("steeringMode".to_owned(), queue_mode);
    }

    // legacy websockets boolean -> transport enum
    if !settings.contains_key("transport")
        && let Some(Value::Bool(websockets)) = settings.get("websockets").cloned()
    {
        settings.insert(
            "transport".to_owned(),
            Value::from(if websockets { "websocket" } else { "sse" }),
        );
        settings.remove("websockets");
    }

    // old skills object format -> array format
    if let Some(Value::Object(skills)) = settings.get("skills").cloned() {
        if let Some(enable) = skills.get("enableSkillCommands")
            && !settings.contains_key("enableSkillCommands")
        {
            settings.insert("enableSkillCommands".to_owned(), enable.clone());
        }
        match skills.get("customDirectories") {
            Some(Value::Array(directories)) if !directories.is_empty() => {
                settings.insert("skills".to_owned(), Value::Array(directories.clone()));
            }
            _ => {
                settings.remove("skills");
            }
        }
    }

    // retry.maxDelayMs -> retry.provider.maxRetryDelayMs
    if let Some(Value::Object(retry)) = settings.get_mut("retry") {
        if let Some(max_delay) = retry
            .get("maxDelayMs")
            .filter(|value| value.is_number())
            .cloned()
        {
            let provider = match retry.get("provider") {
                Some(Value::Object(provider)) => Some(provider.clone()),
                _ => None,
            };
            let configured = provider
                .as_ref()
                .and_then(|provider| provider.get("maxRetryDelayMs"))
                .is_some_and(|value| !value.is_null());
            if !configured {
                let mut provider = provider.unwrap_or_default();
                provider.insert("maxRetryDelayMs".to_owned(), max_delay);
                retry.insert("provider".to_owned(), Value::Object(provider));
            }
        }
        retry.remove("maxDelayMs");
    }

    migrate_renamed_provider_ids(&mut settings);
    settings
}

/// camelCase provider-settings block key, e.g. `claude-sdk-oauth` -> `claudeSdkOauthProvider`.
fn provider_settings_key(provider_id: &str) -> String {
    let mut key = String::new();
    for (index, part) in provider_id.split('-').enumerate() {
        if index == 0 {
            key.push_str(part);
            continue;
        }
        let mut characters = part.chars();
        if let Some(first) = characters.next() {
            key.extend(first.to_uppercase());
            key.push_str(characters.as_str());
        }
    }
    key.push_str("Provider");
    key
}

/// senpi `migrateRenamedProviderIds`: idempotent, and an unrecognised shape is left untouched.
fn migrate_renamed_provider_ids(settings: &mut Settings) {
    use maho_ai::legacy_provider_ids::{
        LEGACY_PROVIDER_IDS, normalize_model_ref, normalize_provider_id,
    };

    for (legacy_id, canonical_id) in LEGACY_PROVIDER_IDS {
        let legacy_key = provider_settings_key(legacy_id);
        let canonical_key = provider_settings_key(canonical_id);
        if settings.contains_key(&legacy_key)
            && !settings.contains_key(&canonical_key)
            && let Some(block) = settings.remove(&legacy_key)
        {
            settings.insert(canonical_key, block);
        }
    }

    if let Some(Value::String(provider)) = settings.get("defaultProvider") {
        let provider = normalize_provider_id(provider);
        settings.insert("defaultProvider".to_owned(), Value::String(provider));
    }
    if let Some(Value::String(model)) = settings.get("defaultModel") {
        let model = normalize_model_ref(model);
        settings.insert("defaultModel".to_owned(), Value::String(model));
    }
    if let Some(Value::Array(favorites)) = settings.get_mut("favoriteModels") {
        for entry in favorites.iter_mut() {
            if let Value::String(model) = entry {
                *model = normalize_model_ref(model);
            }
        }
    }

    for field in [
        "modelThinkingLevels",
        "modelServiceTiers",
        "modelLastOnThinkingLevels",
    ] {
        if let Some(Value::Object(map)) = settings.get(field) {
            let rekeyed = rekey_by_model_ref(map);
            settings.insert(field.to_owned(), Value::Object(rekeyed));
        }
    }

    if let Some(Value::Object(retry)) = settings.get_mut("retry")
        && let Some(Value::Object(chains)) = retry.get("fallbackChains")
    {
        let mut rekeyed = Map::new();
        for (key, rungs) in chains {
            let next_rungs = match rungs {
                Value::Array(rungs) => Value::Array(
                    rungs
                        .iter()
                        .map(|rung| match rung {
                            Value::String(rung) => Value::String(normalize_model_ref(rung)),
                            other => other.clone(),
                        })
                        .collect(),
                ),
                other => other.clone(),
            };
            rekeyed.insert(normalize_model_ref(key), next_rungs);
        }
        retry.insert("fallbackChains".to_owned(), Value::Object(rekeyed));
    }
}

/// Rewrite every `${provider}/${id}` key of a model map through `normalize_model_ref`.
fn rekey_by_model_ref(map: &Map<String, Value>) -> Map<String, Value> {
    let mut out = Map::new();
    for (key, value) in map {
        out.insert(
            maho_ai::legacy_provider_ids::normalize_model_ref(key),
            value.clone(),
        );
    }
    out
}

fn parse_current_settings(current: Option<&str>) -> Result<Settings, String> {
    match current {
        Some(content) => parse_settings_json(content).map(migrate_settings),
        None => Ok(Settings::new()),
    }
}

/// `{ ...base[field], ...nested }` with `null` removing a nested key.
fn nested_into(base: Option<&Value>, nested: &Settings) -> Map<String, Value> {
    let mut object = base.and_then(Value::as_object).cloned().unwrap_or_default();
    for (key, value) in nested {
        if value.is_null() {
            object.remove(key);
        } else {
            object.insert(key.clone(), value.clone());
        }
    }
    object
}

fn values_into(base: &Settings, values: &Settings) -> Settings {
    let mut result = base.clone();
    for (key, value) in values {
        if value.is_null() {
            result.remove(key);
        } else {
            result.insert(key.clone(), value.clone());
        }
    }
    result
}

/// The `getProviderRetrySettings` return shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderRequestRetrySettings {
    pub timeout_ms: Option<u64>,
    pub max_retries: Option<u32>,
    pub max_retry_delay_ms: u64,
}

fn settings_ms(value: &Value) -> Option<u64> {
    value.as_u64().or_else(|| {
        value
            .as_f64()
            .filter(|value| value.is_finite() && *value >= 0.0)
            .map(|value| value.floor() as u64)
    })
}

/// A settings string array; non-string entries are skipped (senpi reads the typed field verbatim).
fn string_list(value: Option<&Value>) -> Option<Vec<String>> {
    value.and_then(Value::as_array).map(|items| {
        items
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect()
    })
}

pub fn serialize_settings(settings: &Settings) -> String {
    let mut serialized = serde_json::to_string_pretty(&Value::Object(settings.clone()))
        .unwrap_or_else(|_| "{}".to_owned());
    serialized.push('\n');
    serialized
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn builtin_extension_allowlist_and_denylist_read_merged_settings() {
        let unset = SettingsManager::in_memory(Settings::new(), false);
        assert_eq!(unset.get_enabled_builtin_extensions(), None);
        assert!(unset.get_disabled_builtin_extensions().is_empty());
        let configured = SettingsManager::in_memory(
            settings(
                r#"{"enabledBuiltinExtensions":["mcp","help"],"disabledBuiltinExtensions":["tps"]}"#,
            ),
            false,
        );
        assert_eq!(
            configured.get_enabled_builtin_extensions(),
            Some(vec!["mcp".to_owned(), "help".to_owned()])
        );
        assert_eq!(
            configured.get_disabled_builtin_extensions(),
            vec!["tps".to_owned()]
        );
    }

    fn settings(json_text: &str) -> Settings {
        parse_settings_json(json_text).expect("settings")
    }

    #[test]
    fn parses_a_plain_object() {
        let parsed = settings("{\"defaultModel\":\"x\",\"tips\":true}");
        assert_eq!(parsed["defaultModel"], "x");
        assert_eq!(parsed["tips"], true);
    }

    #[test]
    fn strips_comments_without_touching_comment_like_text_in_strings() {
        let parsed = settings(
            "{\n  // line comment\n  \"a\": \"// not a comment\",\n  /* block\n     comment */ \"b\": \"/* also not */\"\n}",
        );
        assert_eq!(parsed["a"], "// not a comment");
        assert_eq!(parsed["b"], "/* also not */");
    }

    #[test]
    fn removes_trailing_commas_in_objects_and_arrays() {
        let parsed = settings("{\"a\": [1, 2,],\"b\": {\"c\": 1,},}");
        assert_eq!(parsed["a"], json!([1, 2]));
        assert_eq!(parsed["b"]["c"], 1);
    }

    #[test]
    fn reports_an_unterminated_block_comment_and_a_non_object() {
        assert_eq!(
            parse_settings_json("{\"a\": 1 /* oops").expect_err("error"),
            "Unterminated block comment in settings"
        );
        assert_eq!(
            parse_settings_json("[1]").expect_err("error"),
            "Settings must contain a JSON object"
        );
        assert!(parse_settings_json("{oops}").is_err());
    }

    #[test]
    fn strips_a_byte_order_mark() {
        let parsed = settings("\u{feff}{\"a\":1}");
        assert_eq!(parsed["a"], 1);
    }

    #[test]
    fn model_service_tier_is_global_validated_and_persisted() {
        let mut manager = SettingsManager::in_memory(
            settings(
                r#"{"modelServiceTiers":{"openai/codex":"priority","openai/invalid":"bogus"}}"#,
            ),
            false,
        );
        assert_eq!(
            manager.get_model_service_tier("openai", "codex"),
            Some("priority")
        );
        assert_eq!(manager.get_model_service_tier("openai", "invalid"), None);
        manager
            .set_model_service_tier("openai", "codex", "auto")
            .expect("persist tier");
        assert_eq!(
            manager.get_global()["modelServiceTiers"]["openai/codex"],
            "auto"
        );
        assert!(
            manager
                .set_model_service_tier("openai", "codex", "bogus")
                .is_err()
        );
    }

    #[test]
    fn prefers_jsonc_over_json_when_both_exist() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        let agent = home.join(".omo").join("agent");
        let project = tmp.path().join("proj");
        std::fs::create_dir_all(&agent).expect("mkdir");
        std::fs::create_dir_all(&project).expect("mkdir");
        let home = home.to_string_lossy().into_owned();
        let agent_path = agent.clone();
        let agent = agent.to_string_lossy().into_owned();
        let project = project.to_string_lossy().into_owned();

        assert!(resolve_settings_source(&project, &agent, SettingsScope::Global, &home).is_none());
        assert_eq!(
            get_settings_path(&project, &agent, SettingsScope::Global, &home),
            agent_path.join("settings.json").to_string_lossy()
        );

        std::fs::write(agent_path.join("settings.json"), "{}").expect("write");
        let source = resolve_settings_source(&project, &agent, SettingsScope::Global, &home)
            .expect("source");
        assert_eq!(source.format, SettingsFormat::Json);
        assert_eq!(source.reason, SettingsSourceReason::JsonOnly);

        std::fs::write(agent_path.join("settings.jsonc"), "{}").expect("write");
        let source = resolve_settings_source(&project, &agent, SettingsScope::Global, &home)
            .expect("source");
        assert_eq!(source.format, SettingsFormat::Jsonc);
        assert_eq!(source.reason, SettingsSourceReason::ExplicitJsonc);
        assert_eq!(
            get_settings_path(&project, &agent, SettingsScope::Global, &home),
            agent_path.join("settings.jsonc").to_string_lossy()
        );
    }

    #[test]
    fn rebinding_the_cwd_reads_the_new_project_settings() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        let agent = tmp.path().join("agent");
        std::fs::create_dir_all(&home).expect("mkdir");
        let mut projects = Vec::new();
        for name in ["first", "second"] {
            let project = tmp.path().join(name);
            std::fs::create_dir_all(project.join(config_dir_name())).expect("mkdir");
            std::fs::write(
                project.join(config_dir_name()).join("settings.json"),
                format!("{{\"marker\":\"{name}\"}}"),
            )
            .expect("settings");
            projects.push(project.to_string_lossy().into_owned());
        }
        let mut manager = SettingsManager::create(
            &projects[0],
            &agent.to_string_lossy(),
            &home.to_string_lossy(),
            true,
        );
        assert_eq!(manager.get_string("marker").as_deref(), Some("first"));
        manager.rebind_cwd(&projects[1]);
        assert_eq!(manager.get_string("marker").as_deref(), Some("second"));
    }

    #[test]
    fn a_project_settings_dir_is_found_above_the_cwd() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        let project = tmp.path().join("proj");
        let nested = project.join("a/b");
        std::fs::create_dir_all(&nested).expect("mkdir");
        std::fs::create_dir_all(project.join(".omo")).expect("mkdir");
        std::fs::create_dir_all(&home).expect("mkdir");
        let directory = get_settings_directory(
            &nested.to_string_lossy(),
            "/agent",
            SettingsScope::Project,
            &home.to_string_lossy(),
        );
        assert_eq!(directory, project.join(".omo").to_string_lossy());
    }

    #[test]
    fn in_memory_paths_are_stable() {
        assert_eq!(
            get_in_memory_settings_path(SettingsScope::Global),
            "/__senpi_in_memory__/settings.json"
        );
        assert_eq!(
            get_in_memory_settings_path(SettingsScope::Project),
            "/__senpi_in_memory__/.senpi/settings.json"
        );
    }

    #[test]
    fn self_writes_are_recorded_consumed_and_expire() {
        reset_self_write_tracker_for_tests();
        let clock = std::sync::Arc::new(Mutex::new(1_000u64));
        {
            let clock = std::sync::Arc::clone(&clock);
            set_self_write_tracker_clock_for_tests(Some(Box::new(move || {
                *clock.lock().expect("clock")
            })));
        }
        record_self_write("/p/settings.json", "content");
        let hash = {
            let mut hasher = Sha256::new();
            hasher.update(b"content");
            hex::encode(hasher.finalize())
        };
        assert!(was_self_write("/p/settings.json", &hash));
        assert!(!was_self_write("/p/settings.json", &hash));
        assert!(!was_self_write("/other/settings.json", &hash));

        record_self_write("/p/settings.json", "content");
        *clock.lock().expect("clock") = 1_000 + SELF_WRITE_TTL_MS + 1;
        assert!(!was_self_write("/p/settings.json", &hash));
        set_self_write_tracker_clock_for_tests(None);
        reset_self_write_tracker_for_tests();
    }

    #[test]
    fn the_self_write_tracker_keeps_at_most_eight_hashes_per_path() {
        reset_self_write_tracker_for_tests();
        for index in 0..12 {
            record_self_write("/p/settings.json", &format!("content-{index}"));
        }
        let mut hasher = Sha256::new();
        hasher.update(b"content-0");
        assert!(!was_self_write(
            "/p/settings.json",
            &hex::encode(hasher.finalize())
        ));
        let mut hasher = Sha256::new();
        hasher.update(b"content-11");
        assert!(was_self_write(
            "/p/settings.json",
            &hex::encode(hasher.finalize())
        ));
        reset_self_write_tracker_for_tests();
    }

    #[test]
    fn file_storage_writes_atomically_and_records_a_self_write() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let agent = tmp.path().join("agent").to_string_lossy().into_owned();
        let storage = FileSettingsStorage::new(&agent, &agent, &agent);
        let mut apply = |current: Option<&str>| -> Option<String> {
            assert!(current.is_none());
            Some("{\"a\":1}\n".to_owned())
        };
        storage
            .with_lock(SettingsScope::Global, &mut apply)
            .expect("write");
        let path = storage.settings_path(SettingsScope::Global);
        assert_eq!(std::fs::read_to_string(&path).expect("read"), "{\"a\":1}\n");
        let leftovers: Vec<String> = std::fs::read_dir(Path::new(&path).parent().expect("parent"))
            .expect("read_dir")
            .flatten()
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty());
        let mut hasher = Sha256::new();
        hasher.update(b"{\"a\":1}\n");
        assert!(was_self_write(&path, &hex::encode(hasher.finalize())));
        reset_self_write_tracker_for_tests();
    }

    #[test]
    fn in_memory_storage_keeps_the_latest_content() {
        let storage = InMemorySettingsStorage::default();
        let mut apply = |_current: Option<&str>| Some("{\"a\":1}\n".to_owned());
        storage
            .with_lock(SettingsScope::Global, &mut apply)
            .expect("write");
        assert_eq!(
            storage.content(SettingsScope::Global).as_deref(),
            Some("{\"a\":1}\n")
        );
        assert_eq!(storage.content(SettingsScope::Project), None);
        reset_self_write_tracker_for_tests();
    }

    #[test]
    fn in_memory_seeds_global_settings_and_excludes_project() {
        let mut global = Settings::new();
        global.insert("theme".to_owned(), serde_json::json!("custom"));
        let manager = SettingsManager::in_memory(global.clone(), false);
        assert_eq!(manager.get_global(), &global);
        assert!(manager.get_project().is_empty());
        assert!(!manager.is_project_trusted());
        assert_eq!(
            manager.get_value("theme"),
            Some(&serde_json::json!("custom"))
        );
    }

    #[test]
    fn mermaid_rendering_mode_defaults_to_streaming_and_reads_markdown_mermaid() {
        use crate::settings_shapes::MermaidRenderingMode;
        assert_eq!(
            SettingsManager::in_memory(Settings::new(), false).get_mermaid_rendering_mode(),
            MermaidRenderingMode::Streaming
        );
        assert_eq!(
            SettingsManager::in_memory(settings(r#"{"markdown":{"mermaid":"final"}}"#), false)
                .get_mermaid_rendering_mode(),
            MermaidRenderingMode::Final
        );
        assert_eq!(
            SettingsManager::in_memory(settings(r#"{"markdown":{"mermaid":"off"}}"#), false)
                .get_mermaid_rendering_mode(),
            MermaidRenderingMode::Off
        );
        assert_eq!(
            SettingsManager::in_memory(settings(r#"{"markdown":{"mermaid":"bogus"}}"#), false)
                .get_mermaid_rendering_mode(),
            MermaidRenderingMode::Streaming
        );
    }

    #[test]
    fn code_block_indent_defaults_to_two_spaces() {
        assert_eq!(
            SettingsManager::in_memory(settings("{}"), false).get_code_block_indent(),
            "  "
        );
        assert_eq!(
            SettingsManager::in_memory(
                settings(r#"{"markdown":{"codeBlockIndent":"    "}}"#),
                false
            )
            .get_code_block_indent(),
            "    "
        );
    }

    #[test]
    fn set_mermaid_rendering_mode_keeps_other_markdown_keys() {
        use crate::settings_shapes::MermaidRenderingMode;
        let mut manager = SettingsManager::in_memory(
            settings(r#"{"markdown":{"codeBlockIndent":"    "}}"#),
            false,
        );
        manager
            .set_mermaid_rendering_mode(MermaidRenderingMode::Off)
            .expect("set mermaid");
        assert_eq!(
            manager.get_mermaid_rendering_mode(),
            MermaidRenderingMode::Off
        );
        assert_eq!(
            manager.get_value("markdown"),
            Some(&json!({"codeBlockIndent":"    ","mermaid":"off"}))
        );
    }

    #[test]
    fn terminal_capability_overrides_prefer_settings_then_env() {
        use maho_tui::terminal_capabilities::ImageProtocol;
        let env: HashMap<String, String> = [
            ("PI_IMAGE_PROTOCOL", "KITTY"),
            ("PI_TRUE_COLOR", "0"),
            ("PI_HYPERLINKS", "maybe"),
        ]
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect();
        let from_env = SettingsManager::in_memory(Settings::new(), false)
            .get_terminal_capability_overrides(&env);
        assert_eq!(from_env.images, Some(Some(ImageProtocol::Kitty)));
        assert_eq!(from_env.true_color, Some(false));
        assert_eq!(from_env.hyperlinks, None);
        let configured = SettingsManager::in_memory(
            settings(r#"{"terminal":{"images":false,"trueColor":true,"hyperlinks":true}}"#),
            false,
        );
        let overrides = configured.get_terminal_capability_overrides(&env);
        assert_eq!(overrides.images, Some(None));
        assert_eq!(overrides.true_color, Some(true));
        assert_eq!(overrides.hyperlinks, Some(true));
    }

    #[test]
    fn terminal_renderer_settings_prefer_settings_then_env_and_default_off() {
        let env: HashMap<String, String> =
            [("PI_HARDWARE_CURSOR", "1"), ("PI_CLEAR_ON_SHRINK", "1")]
                .into_iter()
                .map(|(key, value)| (key.to_owned(), value.to_owned()))
                .collect();
        let unset = SettingsManager::in_memory(Settings::new(), false);
        assert!(!unset.get_show_hardware_cursor_with_env(&HashMap::new()));
        assert!(!unset.get_clear_on_shrink_with_env(&HashMap::new()));
        assert!(!unset.get_show_terminal_progress());
        assert!(unset.get_show_hardware_cursor_with_env(&env));
        assert!(unset.get_clear_on_shrink_with_env(&env));
        let configured = SettingsManager::in_memory(
            settings(
                r#"{"showHardwareCursor":false,"terminal":{"clearOnShrink":false,"showTerminalProgress":true}}"#,
            ),
            false,
        );
        assert!(!configured.get_show_hardware_cursor_with_env(&env));
        assert!(!configured.get_clear_on_shrink_with_env(&env));
        assert!(configured.get_show_terminal_progress());
    }

    #[test]
    fn image_auto_resize_defaults_to_true_and_reads_false() {
        let missing = SettingsManager::in_memory(Settings::new(), false);
        assert!(missing.get_image_auto_resize());

        let configured =
            SettingsManager::in_memory(settings(r#"{"images":{"autoResize":false}}"#), false);
        assert!(!configured.get_image_auto_resize());
    }

    #[test]
    fn block_images_defaults_to_false_and_reads_true() {
        assert!(!SettingsManager::in_memory(Settings::new(), false).get_block_images());
        assert!(
            SettingsManager::in_memory(settings(r#"{"images":{"blockImages":true}}"#), false)
                .get_block_images()
        );
    }

    #[test]
    fn max_historical_images_accepts_only_non_negative_integers() {
        assert_eq!(
            SettingsManager::in_memory(Settings::new(), false).get_max_historical_images(),
            None
        );
        assert_eq!(
            SettingsManager::in_memory(settings(r#"{"images":{"maxHistoricalImages":3}}"#), false)
                .get_max_historical_images(),
            Some(3)
        );
        assert_eq!(
            SettingsManager::in_memory(settings(r#"{"images":{"maxHistoricalImages":-1}}"#), false)
                .get_max_historical_images(),
            None
        );
        assert_eq!(
            SettingsManager::in_memory(
                settings(r#"{"images":{"maxHistoricalImages":1.5}}"#),
                false
            )
            .get_max_historical_images(),
            None
        );
    }

    #[test]
    fn provider_concurrency_limit_reads_positive_values_and_notifies_on_change() {
        let mut manager =
            SettingsManager::from_storage(Box::new(InMemorySettingsStorage::default()), true);
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = calls.clone();
        let _subscription = manager.subscribe_to_provider_settings(Arc::new(move |_| {
            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }));
        manager.apply_overrides(&settings(
            r#"{"providers":{"p":{"maxConcurrency":3},"z":{"maxConcurrency":0}}}"#,
        ));
        assert_eq!(manager.get_provider_concurrency_limit("p"), Some(3));
        assert_eq!(manager.get_provider_concurrency_limit("z"), None);
        assert_eq!(manager.get_provider_concurrency_limit("missing"), None);
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn session_overrides_survive_reload_without_writing_storage() {
        let mut manager =
            SettingsManager::from_storage(Box::new(InMemorySettingsStorage::default()), true);
        manager.apply_overrides(&settings(
            r#"{"retry":{"modelFallback":false},"askUser":{"enabled":false}}"#,
        ));
        manager.reload();
        assert_eq!(
            manager
                .get_value("retry")
                .and_then(|value| value.get("modelFallback")),
            Some(&Value::Bool(false))
        );
        assert_eq!(
            manager
                .get_value("askUser")
                .and_then(|value| value.get("enabled")),
            Some(&Value::Bool(false))
        );
        assert!(!manager.global_settings.contains_key("retry"));
    }

    #[test]
    fn the_manager_merges_project_over_global_with_nested_objects() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        let agent = home.join(".omo").join("agent");
        let project = tmp.path().join("proj");
        std::fs::create_dir_all(&agent).expect("mkdir");
        std::fs::create_dir_all(project.join(".omo")).expect("mkdir");
        std::fs::write(
            agent.join("settings.json"),
            "{\"theme\":\"dark\",\"warnings\":{\"tips\":true,\"other\":1},\"onlyGlobal\":1}",
        )
        .expect("write");
        std::fs::write(
            project.join(".omo/settings.json"),
            "{\"theme\":\"light\",\"warnings\":{\"tips\":false}}",
        )
        .expect("write");

        let manager = SettingsManager::create(
            &project.to_string_lossy(),
            &agent.to_string_lossy(),
            &home.to_string_lossy(),
            true,
        );
        assert_eq!(manager.get_string("theme").as_deref(), Some("light"));
        assert_eq!(manager.get()["warnings"]["tips"], false);
        assert_eq!(manager.get()["warnings"]["other"], 1);
        assert_eq!(manager.get()["onlyGlobal"], 1);
        assert!(manager.errors().is_empty());
        assert_eq!(
            manager.settings_path(SettingsScope::Global),
            Some(agent.join("settings.json").to_string_lossy().as_ref())
        );
    }

    #[test]
    fn an_untrusted_project_ignores_project_settings() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        let agent = home.join(".omo").join("agent");
        let project = tmp.path().join("proj");
        std::fs::create_dir_all(&agent).expect("mkdir");
        std::fs::create_dir_all(project.join(".omo")).expect("mkdir");
        std::fs::write(agent.join("settings.json"), "{\"theme\":\"dark\"}").expect("write");
        std::fs::write(project.join(".omo/settings.json"), "{\"theme\":\"light\"}").expect("write");

        let manager = SettingsManager::create(
            &project.to_string_lossy(),
            &agent.to_string_lossy(),
            &home.to_string_lossy(),
            false,
        );
        assert_eq!(manager.get_string("theme").as_deref(), Some("dark"));
        assert!(manager.get_project().is_empty());
        assert!(!manager.is_project_trusted());
        assert!(manager.selected_source(SettingsScope::Project).is_none());
    }

    #[test]
    fn reload_publishes_selected_sources_and_drop_unsubscribes() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().to_string_lossy();
        std::fs::write(tmp.path().join("settings.json"), "{}").expect("settings");
        let mut manager = SettingsManager::create(&path, &path, &path, false);
        let seen = Arc::new(Mutex::new(Vec::new()));
        let captured = seen.clone();
        let subscription = manager.subscribe_to_source_selection(Arc::new(move |source| {
            captured.lock().expect("events").push(source.clone());
        }));
        assert!(seen.lock().expect("events").is_empty());
        std::fs::write(tmp.path().join("settings.jsonc"), "{\"theme\":\"dark\"}").expect("jsonc");
        manager.reload();
        assert_eq!(manager.get_string("theme").as_deref(), Some("dark"));
        assert_eq!(
            seen.lock().expect("events")[0].format,
            SettingsFormat::Jsonc
        );
        assert_eq!(seen.lock().expect("events").len(), 1);
        drop(subscription);
        manager.reload();
        assert_eq!(seen.lock().expect("events").len(), 1);
    }

    #[test]
    fn set_writes_the_scope_file_and_preserves_unknown_keys() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let agent = tmp.path().join("agent");
        std::fs::create_dir_all(&agent).expect("mkdir");
        let agent = agent.to_string_lossy().into_owned();
        std::fs::write(format!("{agent}/settings.json"), "{\"keep\":\"me\"}").expect("write");

        let mut manager = SettingsManager::create(&agent, &agent, &agent, true);
        let mut values = Settings::new();
        values.insert("defaultModel".to_owned(), Value::from("m"));
        manager.set(SettingsScope::Global, &values).expect("set");
        assert_eq!(manager.get_string("defaultModel").as_deref(), Some("m"));
        assert_eq!(manager.get_string("keep").as_deref(), Some("me"));

        let written = std::fs::read_to_string(format!("{agent}/settings.json")).expect("read");
        assert_eq!(
            written,
            "{\n  \"keep\": \"me\",\n  \"defaultModel\": \"m\"\n}\n"
        );

        let mut removal = Settings::new();
        removal.insert("keep".to_owned(), Value::Null);
        manager.set(SettingsScope::Global, &removal).expect("set");
        assert_eq!(manager.get_string("keep"), None);
        reset_self_write_tracker_for_tests();
    }

    #[test]
    fn bookkeeping_keys_move_to_state_json_and_survive_a_config_rewrite() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let agent = tmp.path().join("agent");
        std::fs::create_dir_all(&agent).expect("mkdir");
        let agent = agent.to_string_lossy().into_owned();
        let settings_path = format!("{agent}/settings.json");
        let state_path = format!("{agent}/state.json");
        std::fs::write(
            &settings_path,
            r#"{"theme":"dark","modelThinkingLevels":{"p/m":"high"},"tipsHistory":{"t":1}}"#,
        )
        .expect("write");
        std::fs::write(&state_path, r#"{"tipsHistory":{"kept":2}}"#).expect("state");

        let mut manager = SettingsManager::create(&agent, &agent, &agent, true);
        let on_disk = |path: &str| -> Value {
            serde_json::from_str(&std::fs::read_to_string(path).expect("read")).expect("json")
        };
        assert_eq!(on_disk(&settings_path), serde_json::json!({"theme":"dark"}));
        assert_eq!(
            on_disk(&state_path),
            serde_json::json!({"tipsHistory":{"kept":2},"modelThinkingLevels":{"p/m":"high"}})
        );
        assert_eq!(manager.get()["modelThinkingLevels"]["p/m"], "high");
        assert_eq!(manager.get()["tipsHistory"], serde_json::json!({"kept":2}));

        manager
            .set(
                SettingsScope::Global,
                &serde_json::from_value(serde_json::json!({
                    "modelThinkingLevels": {"p/m": "low"},
                    "defaultThinkingLevel": "low"
                }))
                .expect("values"),
            )
            .expect("set");
        assert_eq!(
            on_disk(&settings_path),
            serde_json::json!({"theme":"dark","defaultThinkingLevel":"low"})
        );
        assert_eq!(on_disk(&state_path)["modelThinkingLevels"]["p/m"], "low");

        // Another process rewrites the whole config file without the bookkeeping keys.
        std::fs::write(&settings_path, r#"{"theme":"light"}"#).expect("rewrite");
        let reloaded = SettingsManager::create(&agent, &agent, &agent, true);
        assert_eq!(reloaded.get()["modelThinkingLevels"]["p/m"], "low");
        assert_eq!(reloaded.get_string("theme").as_deref(), Some("light"));
        reset_self_write_tracker_for_tests();
    }

    #[test]
    fn malformed_state_json_never_blocks_config_load_or_save() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let agent = tmp.path().join("agent");
        std::fs::create_dir_all(&agent).expect("mkdir");
        let agent = agent.to_string_lossy().into_owned();
        std::fs::write(format!("{agent}/settings.json"), r#"{"theme":"dark"}"#).expect("write");
        std::fs::write(format!("{agent}/state.json"), "{ not json").expect("state");

        let mut manager = SettingsManager::create(&agent, &agent, &agent, true);
        assert!(manager.errors().is_empty());
        assert_eq!(manager.get_string("theme").as_deref(), Some("dark"));
        manager
            .set_nested(
                SettingsScope::Global,
                "changelogSeen",
                &Settings::from_iter([("src".to_owned(), Value::from("1.0"))]),
            )
            .expect("state write replaces malformed bookkeeping");
        let state: Value = serde_json::from_str(
            &std::fs::read_to_string(format!("{agent}/state.json")).expect("read"),
        )
        .expect("json");
        assert_eq!(state, serde_json::json!({"changelogSeen":{"src":"1.0"}}));
        reset_self_write_tracker_for_tests();
    }

    #[test]
    fn a_parse_error_is_reported_and_keeps_the_scope_empty() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let agent = tmp.path().join("agent");
        std::fs::create_dir_all(&agent).expect("mkdir");
        let agent = agent.to_string_lossy().into_owned();
        std::fs::write(format!("{agent}/settings.json"), "{ not json }").expect("write");
        let manager = SettingsManager::create(&agent, &agent, &agent, true);
        assert!(manager.get().is_empty());
        assert!(manager.global_settings_load_error().is_some());
        assert_eq!(manager.errors().len(), 1);
        assert_eq!(manager.errors()[0].scope, SettingsScope::Global);
    }

    #[test]
    fn reload_picks_up_an_external_edit() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let agent = tmp.path().join("agent");
        std::fs::create_dir_all(&agent).expect("mkdir");
        let agent = agent.to_string_lossy().into_owned();
        std::fs::write(format!("{agent}/settings.json"), "{\"a\":1}").expect("write");
        let mut manager = SettingsManager::create(&agent, &agent, &agent, true);
        assert_eq!(manager.get_number("a"), Some(1.0));
        std::fs::write(format!("{agent}/settings.json"), "{\"a\":2}").expect("write");
        manager.reload();
        assert_eq!(manager.get_number("a"), Some(2.0));
    }

    #[test]
    fn migrate_renames_queue_mode_websockets_skills_and_retry_max_delay() {
        let migrated = migrate_settings(settings(
            r#"{"queueMode":"all","websockets":true,"skills":{"enableSkillCommands":false,"customDirectories":["/s"]},"retry":{"maxDelayMs":5}}"#,
        ));
        assert_eq!(migrated["steeringMode"], "all");
        assert!(!migrated.contains_key("queueMode"));
        assert_eq!(migrated["transport"], "websocket");
        assert!(!migrated.contains_key("websockets"));
        assert_eq!(migrated["enableSkillCommands"], false);
        assert_eq!(migrated["skills"], json!(["/s"]));
        assert_eq!(migrated["retry"], json!({"provider":{"maxRetryDelayMs":5}}));
    }

    #[test]
    fn session_shutdown_quit_budget_defaults_validates_and_floors() {
        let mut manager = SettingsManager::in_memory(Settings::new(), true);
        assert_eq!(
            manager.get_session_shutdown_quit_budget_ms(),
            Ok(DEFAULT_SESSION_SHUTDOWN_QUIT_BUDGET_MS)
        );
        assert_eq!(
            manager.set_session_shutdown_quit_budget_ms(-1.0),
            Err("Invalid sessionShutdownQuitBudgetMs setting: -1".to_owned())
        );
        assert!(
            manager
                .set_session_shutdown_quit_budget_ms(f64::NAN)
                .is_err()
        );
        assert_eq!(manager.set_session_shutdown_quit_budget_ms(1500.9), Ok(()));
        assert_eq!(manager.get_session_shutdown_quit_budget_ms(), Ok(1_500));
        assert_eq!(manager.set_session_shutdown_quit_budget_ms(0.0), Ok(()));
        assert_eq!(manager.get_session_shutdown_quit_budget_ms(), Ok(0));
    }

    #[test]
    fn migrate_rewrites_legacy_provider_ids() {
        let migrated = migrate_settings(settings(
            r#"{"claudeSdkOauthProvider":{"x":1},"defaultProvider":"openai-codex","defaultModel":"claude-sdk-oauth/opus","favoriteModels":["openai-codex/gpt",1],"modelServiceTiers":{"openai-codex/gpt":"flex"},"retry":{"fallbackChains":{"claude-sdk-oauth/opus":["openai-codex/gpt"]}}}"#,
        ));
        assert_eq!(migrated["anthropicSubscriptionProvider"], json!({"x":1}));
        assert!(!migrated.contains_key("claudeSdkOauthProvider"));
        assert_eq!(migrated["defaultProvider"], "chatgpt-subscription");
        assert_eq!(migrated["defaultModel"], "anthropic-subscription/opus");
        assert_eq!(
            migrated["favoriteModels"],
            json!(["chatgpt-subscription/gpt", 1])
        );
        assert_eq!(
            migrated["modelServiceTiers"],
            json!({"chatgpt-subscription/gpt":"flex"})
        );
        assert_eq!(
            migrated["retry"]["fallbackChains"],
            json!({"anthropic-subscription/opus":["chatgpt-subscription/gpt"]})
        );
    }

    #[test]
    fn a_loaded_file_is_migrated() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let agent = tmp.path().to_string_lossy().into_owned();
        std::fs::write(
            tmp.path().join("settings.json"),
            r#"{"queueMode":"one-at-a-time","defaultModel":"openai-codex/gpt"}"#,
        )
        .expect("write");
        let manager = SettingsManager::create(&agent, &agent, &agent, false);
        assert_eq!(
            manager.get_string("steeringMode").as_deref(),
            Some("one-at-a-time")
        );
        assert_eq!(
            manager.get_string("defaultModel").as_deref(),
            Some("chatgpt-subscription/gpt")
        );
    }

    #[test]
    fn set_project_trusted_loads_and_drops_the_project_scope() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        let agent = home.join(".omo").join("agent");
        let project = tmp.path().join("proj");
        std::fs::create_dir_all(&agent).expect("mkdir");
        std::fs::create_dir_all(project.join(".omo")).expect("mkdir");
        std::fs::write(agent.join("settings.json"), "{\"theme\":\"dark\"}").expect("write");
        std::fs::write(project.join(".omo/settings.json"), "{\"theme\":\"light\"}").expect("write");
        let mut manager = SettingsManager::create(
            &project.to_string_lossy(),
            &agent.to_string_lossy(),
            &home.to_string_lossy(),
            false,
        );
        assert_eq!(manager.get_string("theme").as_deref(), Some("dark"));
        manager.set_project_trusted(true);
        assert!(manager.is_project_trusted());
        assert_eq!(manager.get_string("theme").as_deref(), Some("light"));
        assert!(manager.selected_source(SettingsScope::Project).is_some());
        manager.set_project_trusted(false);
        assert_eq!(manager.get_string("theme").as_deref(), Some("dark"));
        assert!(manager.get_project().is_empty());
    }

    #[test]
    fn agent_stream_timeouts_follow_http_idle_and_provider_overrides() {
        let manager = SettingsManager::in_memory(Settings::new(), false);
        assert_eq!(
            manager.get_agent_stream_idle_timeout_ms(),
            Ok(Some(300_000))
        );
        assert_eq!(
            manager.get_agent_stream_start_timeout_ms(),
            Ok(Some(300_000))
        );
        let manager = SettingsManager::in_memory(settings("{\"httpIdleTimeoutMs\":60000}"), false);
        assert_eq!(
            manager.get_agent_stream_start_timeout_ms(),
            Ok(Some(60_000))
        );
        let manager =
            SettingsManager::in_memory(settings("{\"httpIdleTimeoutMs\":\"disabled\"}"), false);
        assert_eq!(
            (
                manager.get_agent_stream_idle_timeout_ms(),
                manager.get_agent_stream_start_timeout_ms()
            ),
            (Ok(None), Ok(None))
        );
        let manager = SettingsManager::in_memory(
            settings("{\"retry\":{\"provider\":{\"timeoutMs\":1000,\"streamStartTimeoutMs\":0}}}"),
            false,
        );
        assert_eq!(
            (
                manager.get_agent_stream_idle_timeout_ms(),
                manager.get_agent_stream_start_timeout_ms()
            ),
            (Ok(Some(1_000)), Ok(None))
        );
    }

    #[test]
    fn invalid_timeout_settings_are_errors_like_senpi() {
        let manager = SettingsManager::in_memory(
            settings("{\"httpIdleTimeoutMs\":\"5m\",\"websocketConnectTimeoutMs\":-1}"),
            false,
        );
        assert_eq!(
            manager.get_http_idle_timeout_ms(),
            Err("Invalid httpIdleTimeoutMs setting: 5m".to_owned())
        );
        assert_eq!(
            manager.get_agent_stream_idle_timeout_ms(),
            Err("Invalid httpIdleTimeoutMs setting: 5m".to_owned())
        );
        assert_eq!(
            manager.get_websocket_connect_timeout_ms(),
            Err("Invalid websocketConnectTimeoutMs setting: -1".to_owned())
        );
    }

    #[test]
    fn agent_option_getters_default_like_senpi() {
        let manager = SettingsManager::in_memory(Settings::new(), false);
        assert_eq!(
            manager.get_steering_mode(),
            maho_agent::types::QueueMode::All
        );
        assert_eq!(
            manager.get_follow_up_mode(),
            maho_agent::types::QueueMode::OneAtATime
        );
        assert_eq!(manager.get_transport(), maho_ai::types::Transport::Auto);
        assert!(manager.get_abort_server_side_fallback());
        assert_eq!(
            manager.get_provider_retry_settings(),
            ProviderRequestRetrySettings {
                timeout_ms: None,
                max_retries: None,
                max_retry_delay_ms: 60_000
            }
        );
        assert_eq!(manager.get_websocket_connect_timeout_ms(), Ok(None));
        assert_eq!(manager.get_thinking_budgets(), None);
    }

    #[test]
    fn merge_lets_null_overrides_replace_and_keeps_nested_objects() {
        let base = settings("{\"a\":1,\"nested\":{\"x\":1,\"y\":2}}");
        let overrides = settings("{\"a\":null,\"nested\":{\"y\":3}}");
        let merged = deep_merge_settings(&base, &overrides);
        assert_eq!(merged["a"], Value::Null);
        assert_eq!(merged["nested"]["x"], 1);
        assert_eq!(merged["nested"]["y"], 3);
        assert!(is_mergeable_object(&merged["nested"]));
        assert!(!is_mergeable_object(&merged["a"]));
    }

    #[test]
    fn override_fallback_chains_replace_the_base_chain_map_wholesale() {
        let base = settings(
            r#"{"retry":{"modelFallback":true,"fallbackChains":{"a":["x/1"],"b":["x/2"]}}}"#,
        );
        let overrides = settings(r#"{"retry":{"fallbackChains":{"c":["x/3"]}}}"#);
        let merged = deep_merge_settings(&base, &overrides);
        assert_eq!(
            merged["retry"],
            json!({"modelFallback":true,"fallbackChains":{"c":["x/3"]}})
        );
    }

    #[test]
    fn set_compaction_enabled_keeps_sibling_compaction_keys_on_disk_and_in_memory() {
        let mut manager = SettingsManager::in_memory(
            settings(r#"{"compaction":{"reserveTokens":100,"keepRecentTokens":200}}"#),
            false,
        );
        manager
            .set_compaction_enabled(false)
            .expect("set compaction");
        let expected = json!({"reserveTokens":100,"keepRecentTokens":200,"enabled":false});
        assert_eq!(manager.get_value("compaction"), Some(&expected));
        manager.reload();
        assert_eq!(manager.get_value("compaction"), Some(&expected));
    }

    #[test]
    fn an_explicit_setter_wins_over_a_session_override() {
        let mut manager =
            SettingsManager::from_storage(Box::new(InMemorySettingsStorage::default()), true);
        manager.apply_overrides(&settings(
            r#"{"retry":{"modelFallback":false,"maxRetries":1},"theme":"light"}"#,
        ));
        manager
            .set_model_fallback_enabled(true)
            .expect("model fallback");
        assert_eq!(
            manager.get_value("retry"),
            Some(&json!({"modelFallback":true,"maxRetries":1}))
        );
        manager
            .set(SettingsScope::Global, &settings(r#"{"theme":"dark"}"#))
            .expect("theme");
        assert_eq!(manager.get_string("theme").as_deref(), Some("dark"));
    }

    #[test]
    fn retry_setters_write_only_their_global_nested_key() {
        let storage = InMemorySettingsStorage::with_global_content(
            r#"{"retry":{"maxRetries":4,"fallbackChains":{"a":["x/1"],"bad":[1]}}}"#,
        );
        let mut manager = SettingsManager::from_storage(Box::new(storage), true);
        manager.apply_overrides(&settings(r#"{"retry":{"modelFallback":false}}"#));
        manager
            .set_fallback_chain("b", &["x/2".to_owned()])
            .expect("set chain");
        manager.remove_fallback_chain("a").expect("remove chain");
        manager.set_fallback_revert_policy("never").expect("policy");
        assert_eq!(
            manager.get_global()["retry"],
            json!({"maxRetries":4,"fallbackChains":{"b":["x/2"]},"fallbackRevertPolicy":"never"})
        );
        assert_eq!(
            manager
                .get_value("retry")
                .and_then(|retry| retry.get("modelFallback")),
            Some(&Value::Bool(false))
        );
    }

    #[test]
    fn terminal_and_image_getters_read_nested_keys_with_senpi_defaults() {
        let defaults = SettingsManager::in_memory(Settings::new(), false);
        assert!(defaults.get_show_images());
        assert_eq!(defaults.get_image_width_cells(), 60);
        assert_eq!(defaults.get_terminal_mouse(), "whilePending");
        assert!(!defaults.get_show_terminal_progress());
        assert_eq!(defaults.get_autocomplete_max_visible(), 5);
        let configured = SettingsManager::in_memory(
            settings(
                r#"{"terminal":{"showImages":false,"imageWidthCells":12.7,"mouse":"always","clearOnShrink":true,"showTerminalProgress":true},"autocompleteMaxVisible":9,"showHardwareCursor":true}"#,
            ),
            false,
        );
        assert!(!configured.get_show_images());
        assert_eq!(configured.get_image_width_cells(), 12);
        assert_eq!(configured.get_terminal_mouse(), "always");
        assert!(configured.get_clear_on_shrink());
        assert!(configured.get_show_terminal_progress());
        assert_eq!(configured.get_autocomplete_max_visible(), 9);
        assert!(configured.get_show_hardware_cursor());
    }

    #[test]
    fn terminal_and_image_setters_merge_into_their_nested_objects() {
        let mut manager = SettingsManager::in_memory(
            settings(r#"{"terminal":{"images":false},"images":{"maxHistoricalImages":2}}"#),
            false,
        );
        manager.set_show_images(false).expect("show images");
        manager.set_image_width_cells(0).expect("width");
        manager.set_terminal_mouse("off").expect("mouse");
        assert!(manager.set_terminal_mouse("bogus").is_err());
        manager.set_clear_on_shrink(true).expect("shrink");
        manager.set_show_terminal_progress(true).expect("progress");
        manager.set_image_auto_resize(false).expect("auto resize");
        manager.set_block_images(true).expect("block images");
        assert_eq!(
            manager.get_global()["terminal"],
            json!({"images":false,"showImages":false,"imageWidthCells":1,"mouse":"off","clearOnShrink":true,"showTerminalProgress":true})
        );
        assert_eq!(
            manager.get_global()["images"],
            json!({"maxHistoricalImages":2,"autoResize":false,"blockImages":true})
        );
        assert!(!manager.get_image_auto_resize());
        assert!(manager.get_block_images());
    }

    #[test]
    fn ask_user_settings_default_bell_on_and_clamp_the_timeout() {
        assert_eq!(
            SettingsManager::in_memory(Settings::new(), false).get_ask_user_settings(),
            AskUserSettings {
                enabled: true,
                timeout_minutes: 30.0,
                bell: true
            }
        );
        let configured = SettingsManager::in_memory(
            settings(r#"{"askUser":{"enabled":false,"timeoutMinutes":0.5,"bell":false}}"#),
            false,
        );
        assert_eq!(
            configured.get_ask_user_settings(),
            AskUserSettings {
                enabled: false,
                timeout_minutes: 1.0,
                bell: false
            }
        );
    }
}
