use std::{fs, io::Write, os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt}, path::{Path, PathBuf}};
use serde_json::{Map, Value, json};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogLevel { Debug, Info, Warn, Error }
impl LogLevel {
    const fn as_str(self) -> &'static str { match self { Self::Debug => "debug", Self::Info => "info", Self::Warn => "warn", Self::Error => "error" } }
}
pub enum LogEvent<'a> {
    WatcherStarted { target_count: f64 },
    ChangeDetected { registration_id: &'a str, paths: &'a [String], deferred: bool },
    SelfWriteSuppressed { path: &'a str },
    RoutineSettingsChangeSuppressed { path: &'a str },
    GeneratedShimChangeSuppressed { path: &'a str },
    ReloadRequested { reason: &'a str, paths: &'a [String] },
    ReloadDeferred { reason: &'a str },
    ReloadCompleted { duration_ms: f64 },
    ValidationRejected { registration_id: &'a str, error_count: f64 },
    RegistrationRejected { registration_id: &'a str, error_count: f64 },
    RegistrationRejectionSuppressed { registration_id: &'a str },
    WatcherError { path: &'a str, message: &'a str },
    RegistrationAdded { id: &'a str },
    RegistrationRemoved { id: &'a str },
}
impl LogEvent<'_> {
    const fn name(&self) -> &'static str {
        match self {
            Self::WatcherStarted { .. } => "watcher_started", Self::ChangeDetected { .. } => "change_detected",
            Self::SelfWriteSuppressed { .. } => "self_write_suppressed", Self::RoutineSettingsChangeSuppressed { .. } => "routine_settings_change_suppressed",
            Self::GeneratedShimChangeSuppressed { .. } => "generated_shim_change_suppressed", Self::ReloadRequested { .. } => "reload_requested",
            Self::ReloadDeferred { .. } => "reload_deferred", Self::ReloadCompleted { .. } => "reload_completed",
            Self::ValidationRejected { .. } => "validation_rejected", Self::RegistrationRejected { .. } => "registration_rejected",
            Self::RegistrationRejectionSuppressed { .. } => "registration_rejection_suppressed", Self::WatcherError { .. } => "watcher_error",
            Self::RegistrationAdded { .. } => "registration_added", Self::RegistrationRemoved { .. } => "registration_removed",
        }
    }
}
#[derive(Debug, PartialEq, Eq)]
pub struct ConfigReloadLogStatus { pub written: bool, pub disabled: bool }
pub struct ConfigReloadLogger {
    path: PathBuf, max_bytes: u64, disabled: bool, sensitive_path: regex::Regex, sensitive_text: regex::Regex,
}
impl ConfigReloadLogger {
    pub fn new(agent_dir: &Path, max_bytes: Option<u64>) -> Result<Self, regex::Error> {
        Ok(Self { path: agent_dir.join("logs/config-reload.log"), max_bytes: max_bytes.filter(|value| *value > 0).unwrap_or(5 * 1024 * 1024), disabled: false,
            sensitive_path: regex::Regex::new(r"(?i)(^|[/\\])(?:auth\.json|credentials?(?:\.[^/\\]+)?)$")?,
            sensitive_text: regex::Regex::new(r#"(?i)((?:authorization\s*[:=]\s*(?:bearer|basic)\s+)|(?:bearer\s+)|(?:[?&](?:api[_-]?key|token|secret|password|auth(?:orization)?)=))[^\s&,"'}\]]+"#)? })
    }
    fn safe_text(&self, text: &str) -> String {
        let redacted = self.sensitive_text.replace_all(text, "${1}[redacted]");
        if redacted.encode_utf16().count() <= 200 { return redacted.into_owned(); }
        let mut units: usize = 0;
        let prefix: String = redacted.chars().take_while(|character| { units = units.saturating_add(character.len_utf16()); units <= 197 }).collect();
        format!("{prefix}...")
    }
    fn safe_paths(&self, paths: &[String]) -> Value {
        Value::Array(paths.iter().filter(|path| !self.sensitive_path.is_match(path)).map(|path| Value::String(self.safe_text(path))).collect())
    }
    fn add_path(&self, entry: &mut Map<String, Value>, path: &str) {
        if !self.sensitive_path.is_match(path) { entry.insert("path".into(), self.safe_text(path).into()); }
    }
    pub fn log(&mut self, level: LogLevel, event: LogEvent<'_>) -> ConfigReloadLogStatus {
        if self.disabled { return ConfigReloadLogStatus { written: false, disabled: true }; }
        let mut entry = Map::new();
        entry.insert("ts".into(), chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true).into());
        entry.insert("level".into(), level.as_str().into());
        entry.insert("event".into(), event.name().into());
        match event {
            LogEvent::WatcherStarted { target_count } => { entry.insert("targetCount".into(), finite_number(target_count)); },
            LogEvent::ChangeDetected { registration_id, paths, deferred } => {
                entry.insert("registrationId".into(), self.safe_text(registration_id).into()); entry.insert("paths".into(), self.safe_paths(paths)); entry.insert("deferred".into(), deferred.into());
            },
            LogEvent::SelfWriteSuppressed { path } | LogEvent::RoutineSettingsChangeSuppressed { path } | LogEvent::GeneratedShimChangeSuppressed { path } => self.add_path(&mut entry, path),
            LogEvent::ReloadRequested { reason, paths } => { entry.insert("reason".into(), self.safe_text(reason).into()); entry.insert("paths".into(), self.safe_paths(paths)); },
            LogEvent::ReloadDeferred { reason } => { entry.insert("reason".into(), self.safe_text(reason).into()); },
            LogEvent::ReloadCompleted { duration_ms } => { entry.insert("durationMs".into(), finite_number(duration_ms)); },
            LogEvent::ValidationRejected { registration_id, error_count } | LogEvent::RegistrationRejected { registration_id, error_count } => {
                entry.insert("registrationId".into(), self.safe_text(registration_id).into()); entry.insert("errorCount".into(), finite_number(error_count));
            },
            LogEvent::WatcherError { path, message } => { self.add_path(&mut entry, path); entry.insert("message".into(), self.safe_text(message).into()); },
            LogEvent::RegistrationAdded { id } | LogEvent::RegistrationRemoved { id } => { entry.insert("id".into(), self.safe_text(id).into()); },
            LogEvent::RegistrationRejectionSuppressed { .. } => {},
        }
        let text = format!("{}\n", Value::Object(entry));
        match self.write_line(&text) {
            Ok(()) => ConfigReloadLogStatus { written: true, disabled: false },
            Err(_) => { self.disabled = true; ConfigReloadLogStatus { written: false, disabled: true } },
        }
    }
    fn write_line(&self, text: &str) -> std::io::Result<()> {
        if let Some(parent) = self.path.parent() { fs::DirBuilder::new().recursive(true).mode(0o700).create(parent)?; }
        let incoming = u64::try_from(text.len()).map_err(std::io::Error::other)?;
        let rotate = match fs::metadata(&self.path) {
            Ok(metadata) => metadata.len().saturating_add(incoming) > self.max_bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => return Err(error),
        };
        if rotate {
            let previous = self.path.with_file_name("config-reload.log.1");
            match fs::remove_file(&previous) { Ok(()) => {}, Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}, Err(error) => return Err(error) }
            fs::rename(&self.path, &previous)?;
            fs::set_permissions(previous, fs::Permissions::from_mode(0o600))?;
        }
        let mut file = fs::OpenOptions::new().append(true).create(true).mode(0o600).open(&self.path)?;
        file.write_all(text.as_bytes())?;
        fs::set_permissions(&self.path, fs::Permissions::from_mode(0o600))
    }
}
fn finite_number(number: f64) -> Value { if number.is_finite() { json!(number) } else { json!(0) } }
