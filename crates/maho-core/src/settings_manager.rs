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
    FILE_STORAGE_SYNC_LOCK_BUDGET_MS, CredentialStoreBusyError, acquire_lock_sync,
};
use crate::nearest_parent_config::find_nearest_parent_config_dir;
use crate::paths::{PathInputOptions, resolve_path};
use crate::text::strip_bom;

pub const DEFAULT_STREAM_START_TIMEOUT_MS: u64 = 300_000;
pub const DEFAULT_PROVIDER_STREAM_RETRY_TIMEOUT_MS: u64 = 30_000;
pub const DEFAULT_SESSION_SHUTDOWN_HANDLER_WARN_MS: u64 = 2_000;
pub const DEFAULT_SESSION_SHUTDOWN_HANDLER_TIMEOUT_MS: u64 = 10_000;
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
        self.listeners.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
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
                without_comments.push(if content[index] == '\n' || content[index] == '\r' { content[index] } else { ' ' });
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
    TRACKER.get_or_init(|| Mutex::new(SelfWriteTracker { writes_by_path: HashMap::new() }))
}

type Clock = Box<dyn Fn() -> u64 + Send + Sync>;

fn self_write_clock() -> &'static Mutex<Clock> {
    static CLOCK: OnceLock<Mutex<Clock>> = OnceLock::new();
    CLOCK.get_or_init(|| Mutex::new(Box::new(default_clock)))
}

fn default_clock() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|duration| duration.as_millis() as u64).unwrap_or(0)
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
    let writes = tracker.writes_by_path.entry(abs_path.to_owned()).or_default();
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
    let Some(writes) = tracker.writes_by_path.get_mut(abs_path) else { return false };
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
    self_writes().lock().expect("self-write lock").writes_by_path.clear();
}

pub fn set_self_write_tracker_clock_for_tests(clock: Option<Clock>) {
    let mut stored = self_write_clock().lock().expect("self-write clock lock");
    *stored = clock.unwrap_or_else(|| Box::new(default_clock));
}

pub fn get_settings_directory(cwd: &str, agent_dir: &str, scope: SettingsScope, home_dir: &str) -> String {
    if scope == SettingsScope::Global {
        return resolve_path(agent_dir, agent_dir, &PathInputOptions::default());
    }
    let resolved_cwd = resolve_path(cwd, cwd, &PathInputOptions::default());
    find_nearest_parent_config_dir(&resolved_cwd, home_dir, &config_dir_name(), None, None)
        .unwrap_or_else(|| Path::new(&resolved_cwd).join(config_dir_name()).to_string_lossy().into_owned())
}

/// Resolve the existing settings source, preferring JSONC when both formats exist.
pub fn resolve_settings_source(
    cwd: &str,
    agent_dir: &str,
    scope: SettingsScope,
    home_dir: &str,
) -> Option<SettingsSourceSelection> {
    let directory = get_settings_directory(cwd, agent_dir, scope, home_dir);
    let jsonc_path = Path::new(&directory).join("settings.jsonc").to_string_lossy().into_owned();
    if Path::new(&jsonc_path).exists() {
        return Some(SettingsSourceSelection {
            path: jsonc_path,
            format: SettingsFormat::Jsonc,
            reason: SettingsSourceReason::ExplicitJsonc,
            scope,
        });
    }
    let json_path = Path::new(&directory).join("settings.json").to_string_lossy().into_owned();
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

pub fn get_settings_path(cwd: &str, agent_dir: &str, scope: SettingsScope, home_dir: &str) -> String {
    resolve_settings_source(cwd, agent_dir, scope, home_dir).map(|source| source.path).unwrap_or_else(|| {
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
    fn with_lock(&self, scope: SettingsScope, update: &mut dyn FnMut(Option<&str>) -> Option<String>) -> Result<(), String>;
    fn select_source(&self, _scope: SettingsScope) -> Option<SettingsSourceSelection> {
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
    fn select_source(&self, scope: SettingsScope) -> Option<SettingsSourceSelection> {
        let source = resolve_settings_source(&self.cwd, &self.agent_dir, scope, &self.home_dir);
        let path = source.as_ref().map(|source| source.path.clone()).unwrap_or_else(|| {
            Path::new(&get_settings_directory(&self.cwd, &self.agent_dir, scope, &self.home_dir))
                .join("settings.json")
                .to_string_lossy()
                .into_owned()
        });
        match scope {
            SettingsScope::Global => *self.global_settings_path.lock().expect("settings path lock") = path,
            SettingsScope::Project => *self.project_settings_path.lock().expect("settings path lock") = path,
        }
        source
    }

    fn with_lock(&self, scope: SettingsScope, update: &mut dyn FnMut(Option<&str>) -> Option<String>) -> Result<(), String> {
        let path = self.settings_path(scope);
        let directory = Path::new(&path).parent().map(|parent| parent.to_string_lossy().into_owned()).unwrap_or_default();

        // Read without the lock: writers publish atomically via temp+rename below, so a reader can
        // never observe partial content, and read-only callers skip lock acquisition entirely.
        let current = std::fs::read_to_string(&path).ok();
        let mut next = update(current.as_deref());
        if next.is_none() {
            return Ok(());
        }
        if !Path::new(&directory).exists() {
            std::fs::create_dir_all(&directory).map_err(|error| format!("Failed to write settings {path}: {error}"))?;
        }
        let _guard = acquire_lock_sync(&path, FILE_STORAGE_SYNC_LOCK_BUDGET_MS)
            .map_err(|error: CredentialStoreBusyError| error.message())?;
        let under_lock = std::fs::read_to_string(&path).ok();
        if under_lock != current {
            // Lost a write race: re-merge against the winner's content under the lock.
            next = update(under_lock.as_deref());
        }
        if let Some(next) = next {
            let temp_path = format!("{path}.{}.{}.tmp", std::process::id(), uuid::Uuid::new_v4());
            std::fs::write(&temp_path, &next).map_err(|error| {
                let _ = std::fs::remove_file(&temp_path);
                format!("Failed to write settings {path}: {error}")
            })?;
            record_self_write(&path, &next);
            std::fs::rename(&temp_path, &path).map_err(|error| {
                let _ = std::fs::remove_file(&temp_path);
                format!("Failed to write settings {path}: {error}")
            })?;
        }
        Ok(())
    }
}

pub struct InMemorySettingsStorage {
    global: Mutex<Option<String>>,
    project: Mutex<Option<String>>,
}

impl Default for InMemorySettingsStorage {
    fn default() -> Self {
        Self { global: Mutex::new(None), project: Mutex::new(None) }
    }
}

impl InMemorySettingsStorage {
    pub fn content(&self, scope: SettingsScope) -> Option<String> {
        match scope {
            SettingsScope::Global => self.global.lock().expect("in-memory settings lock").clone(),
            SettingsScope::Project => self.project.lock().expect("in-memory settings lock").clone(),
        }
    }
}

impl SettingsStorage for InMemorySettingsStorage {
    fn with_lock(&self, scope: SettingsScope, update: &mut dyn FnMut(Option<&str>) -> Option<String>) -> Result<(), String> {
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
}

pub fn is_mergeable_object(value: &Value) -> bool {
    matches!(value, Value::Object(_))
}

pub fn deep_merge_objects(base: &Settings, overrides: &Settings) -> Settings {
    let mut result = base.clone();
    for (key, override_value) in overrides {
        if override_value.is_null() {
            continue;
        }
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
pub fn deep_merge_settings(base: &Settings, overrides: &Settings) -> Settings {
    deep_merge_objects(base, overrides)
}

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
}

impl SettingsManager {
    pub fn from_storage(storage: Box<dyn SettingsStorage>, project_trusted: bool) -> SettingsManager {
        let mut settings_paths: HashMap<SettingsScope, String> = HashMap::new();
        let mut selected_sources: HashMap<SettingsScope, SettingsSourceSelection> = HashMap::new();
        if let Some(source) = storage.select_source(SettingsScope::Global) {
            settings_paths.insert(SettingsScope::Global, source.path.clone());
            selected_sources.insert(SettingsScope::Global, source);
        }
        let global_load = Self::try_load_from_storage(storage.as_ref(), SettingsScope::Global, true);
        if project_trusted
            && let Some(source) = storage.select_source(SettingsScope::Project) {
                settings_paths.insert(SettingsScope::Project, source.path.clone());
                selected_sources.insert(SettingsScope::Project, source);
            }
        let project_load = Self::try_load_from_storage(storage.as_ref(), SettingsScope::Project, project_trusted);

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
        }
    }

    pub fn create(cwd: &str, agent_dir: &str, home_dir: &str, project_trusted: bool) -> SettingsManager {
        let storage = FileSettingsStorage::new(cwd, agent_dir, home_dir);
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
            && error.is_none() {
                error = Some(storage_error);
            }
        (loaded, error)
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

    pub fn subscribe_to_source_selection(&self, listener: SettingsSourceListener) -> SettingsSourceSubscription {
        self.source_listeners.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(listener.clone());
        SettingsSourceSubscription { listeners: self.source_listeners.clone(), listener }
    }

    fn select_and_publish_source(&mut self, scope: SettingsScope) {
        let Some(source) = self.storage.select_source(scope) else {
            self.selected_sources.remove(&scope);
            return;
        };
        self.settings_paths.insert(scope, source.path.clone());
        self.selected_sources.insert(scope, source.clone());
        let listeners = self.source_listeners.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
        for listener in listeners { listener(&source); }
    }

    pub fn get_value(&self, key: &str) -> Option<&Value> {
        self.settings.get(key)
    }

    pub fn get_string(&self, key: &str) -> Option<String> {
        self.settings.get(key).and_then(Value::as_str).map(str::to_owned)
    }

    pub fn get_bool(&self, key: &str) -> Option<bool> {
        self.settings.get(key).and_then(Value::as_bool)
    }

    pub fn get_number(&self, key: &str) -> Option<f64> {
        self.settings.get(key).and_then(Value::as_f64)
    }

    pub fn apply_overrides(&mut self, overrides: &Settings) {
        self.overrides = deep_merge_settings(&self.overrides, overrides);
        self.settings = deep_merge_settings(&deep_merge_settings(&self.global_settings, &self.project_settings), &self.overrides);
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
            if let Some(max_retries) = retry.and_then(|value| value.get("maxRetries"))
                .and_then(Value::as_u64).and_then(|value| u32::try_from(value).ok())
            {
                profile.turn.max_retries = max_retries;
            }
            if let Some(base_delay) = retry.and_then(|value| value.get("baseDelayMs")).and_then(Value::as_f64) {
                profile.turn.backoff.base_delay_ms = base_delay;
            }
        }
        let validated = crate::retry_fallback::profile_override::validate_retry_provider_overrides(
            retry.and_then(|value| value.get("providers")),
            &provider.map(|provider| std::collections::HashSet::from([provider.id().to_owned()])).unwrap_or_default(),
            Some(&provider.filter(|_| matches!(profile.turn.server_hint,
                maho_ai::utils::retry_profile::types::RetryServerHintPolicy::Tiered { .. }))
                .map(|provider| std::collections::HashSet::from([provider.id().to_owned()])).unwrap_or_default()),
        );
        let overrides = provider.and_then(|provider| validated.overrides.get(provider.id())?.get("turn"));
        if let Some(max_retries) = overrides.and_then(|value| value.get("maxRetries"))
            .and_then(Value::as_u64).and_then(|value| u32::try_from(value).ok())
        {
            profile.turn.max_retries = max_retries;
        }
        if let Some(base_delay) = overrides.and_then(|value| value.get("baseDelayMs")).and_then(Value::as_f64) {
            profile.turn.backoff.base_delay_ms = base_delay;
        }
        if let Some(growth) = overrides.and_then(|value| value.get("growthFactor")).and_then(Value::as_f64) {
            profile.turn.backoff.growth_factor = growth;
        }
        if let Some(cap) = overrides.and_then(|value| value.get("perAttemptCapMs")) {
            profile.turn.backoff.per_attempt_cap_ms = cap.as_f64();
        }
        if let Some(jitter) = overrides.and_then(|value| value.get("jitter")) {
            use maho_ai::utils::retry_profile::types::RetryJitterPolicy;
            profile.turn.backoff.jitter = match jitter.get("mode").and_then(Value::as_str) {
                Some("additive") => RetryJitterPolicy::Additive { ratio: jitter["ratio"].as_f64().expect("validated ratio") },
                Some("subtractive") => RetryJitterPolicy::Subtractive { ratio: jitter["ratio"].as_f64().expect("validated ratio") },
                _ => RetryJitterPolicy::None,
            };
        }
        if let Some(cap) = overrides.and_then(|value| value.get("serverHintMaxDelayMs"))
            && let maho_ai::utils::retry_profile::types::RetryServerHintPolicy::Override { ceiling, .. } = &mut profile.turn.server_hint
        {
            ceiling.max_delay_ms = cap.as_u64();
        }
        profile.turn.enabled = retry.and_then(|value| value.get("enabled")).and_then(Value::as_bool).unwrap_or(true)
            && overrides.and_then(|value| value.get("enabled")).and_then(Value::as_bool).unwrap_or(profile.turn.enabled);
        profile
    }

    /// Writes values into the given scope and recomputes the merged view.
    pub fn set(&mut self, scope: SettingsScope, values: &Settings) -> Result<(), String> {
        let values = values.clone();
        let mut write_error: Option<String> = None;
        let mut apply = |current: Option<&str>| -> Option<String> {
            let mut parsed = match current {
                Some(content) => match parse_settings_json(content) {
                    Ok(parsed) => parsed,
                    Err(error) => {
                        write_error = Some(error);
                        return None;
                    }
                },
                None => Settings::new(),
            };
            for (key, value) in &values {
                if value.is_null() {
                    parsed.remove(key);
                } else {
                    parsed.insert(key.clone(), value.clone());
                }
            }
            Some(serialize_settings(&parsed))
        };
        self.storage.with_lock(scope, &mut apply)?;
        if let Some(error) = write_error {
            return Err(error);
        }
        match scope {
            SettingsScope::Global => {
                self.global_settings = values_into(&self.global_settings, &values);
                self.settings = deep_merge_settings(&deep_merge_settings(&self.global_settings, &self.project_settings), &self.overrides);
            }
            SettingsScope::Project => {
                self.project_settings = values_into(&self.project_settings, &values);
                self.settings = deep_merge_settings(&deep_merge_settings(&self.global_settings, &self.project_settings), &self.overrides);
            }
        }
        Ok(())
    }

    pub fn reload(&mut self) {
        self.select_and_publish_source(SettingsScope::Global);
        let global_load = Self::try_load_from_storage(self.storage.as_ref(), SettingsScope::Global, true);
        if self.project_trusted { self.select_and_publish_source(SettingsScope::Project); }
        let project_load = Self::try_load_from_storage(self.storage.as_ref(), SettingsScope::Project, self.project_trusted);
        self.global_settings = global_load.0;
        self.project_settings = project_load.0;
        self.global_settings_load_error = global_load.1;
        self.project_settings_load_error = project_load.1;
        self.settings = deep_merge_settings(&deep_merge_settings(&self.global_settings, &self.project_settings), &self.overrides);
    }
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

pub fn serialize_settings(settings: &Settings) -> String {
    let mut serialized = serde_json::to_string_pretty(&Value::Object(settings.clone())).unwrap_or_else(|_| "{}".to_owned());
    serialized.push('\n');
    serialized
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
        let parsed = settings("{\n  // line comment\n  \"a\": \"// not a comment\",\n  /* block\n     comment */ \"b\": \"/* also not */\"\n}");
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
        assert_eq!(parse_settings_json("{\"a\": 1 /* oops").expect_err("error"), "Unterminated block comment in settings");
        assert_eq!(parse_settings_json("[1]").expect_err("error"), "Settings must contain a JSON object");
        assert!(parse_settings_json("{oops}").is_err());
    }

    #[test]
    fn strips_a_byte_order_mark() {
        let parsed = settings("\u{feff}{\"a\":1}");
        assert_eq!(parsed["a"], 1);
    }

    #[test]
    fn prefers_jsonc_over_json_when_both_exist() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        let agent = home.join(".maho/agent");
        let project = tmp.path().join("proj");
        std::fs::create_dir_all(&agent).expect("mkdir");
        std::fs::create_dir_all(&project).expect("mkdir");
        let home = home.to_string_lossy().into_owned();
        let agent = agent.to_string_lossy().into_owned();
        let project = project.to_string_lossy().into_owned();

        assert!(resolve_settings_source(&project, &agent, SettingsScope::Global, &home).is_none());
        assert_eq!(
            get_settings_path(&project, &agent, SettingsScope::Global, &home),
            format!("{agent}/settings.json")
        );

        std::fs::write(format!("{agent}/settings.json"), "{}").expect("write");
        let source = resolve_settings_source(&project, &agent, SettingsScope::Global, &home).expect("source");
        assert_eq!(source.format, SettingsFormat::Json);
        assert_eq!(source.reason, SettingsSourceReason::JsonOnly);

        std::fs::write(format!("{agent}/settings.jsonc"), "{}").expect("write");
        let source = resolve_settings_source(&project, &agent, SettingsScope::Global, &home).expect("source");
        assert_eq!(source.format, SettingsFormat::Jsonc);
        assert_eq!(source.reason, SettingsSourceReason::ExplicitJsonc);
        assert_eq!(get_settings_path(&project, &agent, SettingsScope::Global, &home), format!("{agent}/settings.jsonc"));
    }

    #[test]
    fn a_project_settings_dir_is_found_above_the_cwd() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        let project = tmp.path().join("proj");
        let nested = project.join("a/b");
        std::fs::create_dir_all(&nested).expect("mkdir");
        std::fs::create_dir_all(project.join(".maho")).expect("mkdir");
        std::fs::create_dir_all(&home).expect("mkdir");
        let directory = get_settings_directory(
            &nested.to_string_lossy(),
            "/agent",
            SettingsScope::Project,
            &home.to_string_lossy(),
        );
        assert_eq!(directory, project.join(".maho").to_string_lossy());
    }

    #[test]
    fn in_memory_paths_are_stable() {
        assert_eq!(get_in_memory_settings_path(SettingsScope::Global), "/__senpi_in_memory__/settings.json");
        assert_eq!(get_in_memory_settings_path(SettingsScope::Project), "/__senpi_in_memory__/.senpi/settings.json");
    }

    #[test]
    fn self_writes_are_recorded_consumed_and_expire() {
        reset_self_write_tracker_for_tests();
        let clock = std::sync::Arc::new(Mutex::new(1_000u64));
        {
            let clock = std::sync::Arc::clone(&clock);
            set_self_write_tracker_clock_for_tests(Some(Box::new(move || *clock.lock().expect("clock"))));
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
        assert!(!was_self_write("/p/settings.json", &hex::encode(hasher.finalize())));
        let mut hasher = Sha256::new();
        hasher.update(b"content-11");
        assert!(was_self_write("/p/settings.json", &hex::encode(hasher.finalize())));
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
        storage.with_lock(SettingsScope::Global, &mut apply).expect("write");
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
        storage.with_lock(SettingsScope::Global, &mut apply).expect("write");
        assert_eq!(storage.content(SettingsScope::Global).as_deref(), Some("{\"a\":1}\n"));
        assert_eq!(storage.content(SettingsScope::Project), None);
        reset_self_write_tracker_for_tests();
    }

    #[test]
    fn session_overrides_survive_reload_without_writing_storage() {
        let mut manager = SettingsManager::from_storage(Box::new(InMemorySettingsStorage::default()), true);
        manager.apply_overrides(&settings(r#"{"retry":{"modelFallback":false},"askUser":{"enabled":false}}"#));
        manager.reload();
        assert_eq!(manager.get_value("retry").and_then(|value| value.get("modelFallback")), Some(&Value::Bool(false)));
        assert_eq!(manager.get_value("askUser").and_then(|value| value.get("enabled")), Some(&Value::Bool(false)));
        assert!(!manager.global_settings.contains_key("retry"));
    }

    #[test]
    fn the_manager_merges_project_over_global_with_nested_objects() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        let agent = home.join(".maho/agent");
        let project = tmp.path().join("proj");
        std::fs::create_dir_all(&agent).expect("mkdir");
        std::fs::create_dir_all(project.join(".maho")).expect("mkdir");
        std::fs::write(agent.join("settings.json"), "{\"theme\":\"dark\",\"warnings\":{\"tips\":true,\"other\":1},\"onlyGlobal\":1}").expect("write");
        std::fs::write(project.join(".maho/settings.json"), "{\"theme\":\"light\",\"warnings\":{\"tips\":false}}").expect("write");

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
        assert_eq!(manager.settings_path(SettingsScope::Global), Some(agent.join("settings.json").to_string_lossy().as_ref()));
    }

    #[test]
    fn an_untrusted_project_ignores_project_settings() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        let agent = home.join(".maho/agent");
        let project = tmp.path().join("proj");
        std::fs::create_dir_all(&agent).expect("mkdir");
        std::fs::create_dir_all(project.join(".maho")).expect("mkdir");
        std::fs::write(agent.join("settings.json"), "{\"theme\":\"dark\"}").expect("write");
        std::fs::write(project.join(".maho/settings.json"), "{\"theme\":\"light\"}").expect("write");

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
        assert_eq!(seen.lock().expect("events")[0].format, SettingsFormat::Jsonc);
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
        assert_eq!(written, "{\n  \"keep\": \"me\",\n  \"defaultModel\": \"m\"\n}\n");

        let mut removal = Settings::new();
        removal.insert("keep".to_owned(), Value::Null);
        manager.set(SettingsScope::Global, &removal).expect("set");
        assert_eq!(manager.get_string("keep"), None);
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
    fn merge_ignores_null_overrides_and_keeps_nested_objects() {
        let base = settings("{\"a\":1,\"nested\":{\"x\":1,\"y\":2}}");
        let overrides = settings("{\"a\":null,\"nested\":{\"y\":3}}");
        let merged = deep_merge_settings(&base, &overrides);
        assert_eq!(merged["a"], 1);
        assert_eq!(merged["nested"]["x"], 1);
        assert_eq!(merged["nested"]["y"], 3);
        assert!(is_mergeable_object(&merged["nested"]));
        assert!(!is_mergeable_object(&merged["a"]));
    }
}
