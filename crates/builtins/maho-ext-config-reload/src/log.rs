use serde_json::{Map, Value, json};
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogLevel {
    Debug,
    Info,
    Warn,
    Error,
}
impl LogLevel {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Debug => "debug",
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
        }
    }
}
pub enum LogEvent<'a> {
    WatcherStarted {
        target_count: f64,
    },
    ChangeDetected {
        registration_id: &'a str,
        paths: &'a [String],
        deferred: bool,
    },
    SelfWriteSuppressed {
        path: &'a str,
    },
    RoutineSettingsChangeSuppressed {
        path: &'a str,
    },
    GeneratedShimChangeSuppressed {
        path: &'a str,
    },
    ReloadRequested {
        reason: &'a str,
        paths: &'a [String],
    },
    ReloadDeferred {
        reason: &'a str,
    },
    ReloadCompleted {
        duration_ms: f64,
    },
    ReloadLoopStopped {
        paths: &'a [String],
        reloads: f64,
    },
    ValidationRejected {
        registration_id: &'a str,
        error_count: f64,
    },
    RegistrationRejected {
        registration_id: &'a str,
        error_count: f64,
    },
    RegistrationRejectionSuppressed {
        registration_id: &'a str,
    },
    WatcherError {
        path: &'a str,
        message: &'a str,
    },
    RegistrationAdded {
        id: &'a str,
    },
    RegistrationRemoved {
        id: &'a str,
    },
}
impl LogEvent<'_> {
    const fn name(&self) -> &'static str {
        match self {
            Self::WatcherStarted { .. } => "watcher_started",
            Self::ChangeDetected { .. } => "change_detected",
            Self::SelfWriteSuppressed { .. } => "self_write_suppressed",
            Self::RoutineSettingsChangeSuppressed { .. } => "routine_settings_change_suppressed",
            Self::GeneratedShimChangeSuppressed { .. } => "generated_shim_change_suppressed",
            Self::ReloadRequested { .. } => "reload_requested",
            Self::ReloadDeferred { .. } => "reload_deferred",
            Self::ReloadCompleted { .. } => "reload_completed",
            Self::ReloadLoopStopped { .. } => "reload_loop_stopped",
            Self::ValidationRejected { .. } => "validation_rejected",
            Self::RegistrationRejected { .. } => "registration_rejected",
            Self::RegistrationRejectionSuppressed { .. } => "registration_rejection_suppressed",
            Self::WatcherError { .. } => "watcher_error",
            Self::RegistrationAdded { .. } => "registration_added",
            Self::RegistrationRemoved { .. } => "registration_removed",
        }
    }
}
#[derive(Debug, PartialEq, Eq)]
pub struct ConfigReloadLogStatus {
    pub written: bool,
    pub disabled: bool,
}
// Compiled once; loggers are rebuilt on every deferred-reload recheck.
static SENSITIVE_PATH: std::sync::LazyLock<Result<regex::Regex, regex::Error>> =
    std::sync::LazyLock::new(|| {
        regex::Regex::new(r"(?i)(^|[/\\])(?:auth\.json|credentials?(?:\.[^/\\]+)?)$")
    });
static SENSITIVE_TEXT: std::sync::LazyLock<Result<regex::Regex, regex::Error>> =
    std::sync::LazyLock::new(|| {
        regex::Regex::new(
            r#"(?i)((?:authorization\s*[:=]\s*(?:bearer|basic)\s+)|(?:bearer\s+)|(?:[?&](?:api[_-]?key|token|secret|password|auth(?:orization)?)=))[^\s&,"'}\]]+"#,
        )
    });
/// A failed sink waits this long before trying the file again (`LOG_SINK_RETRY_MS`).
pub const LOG_SINK_RETRY_MS: u64 = 5_000;
const ROTATION_LOCK_STALE_MS: u128 = 10_000;
pub type LogClock = Box<dyn Fn() -> u64 + Send>;
fn system_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
        })
}
pub struct ConfigReloadLogger {
    path: PathBuf,
    max_bytes: u64,
    retry_at: u64,
    now_ms: LogClock,
    sensitive_path: regex::Regex,
    sensitive_text: regex::Regex,
}
impl ConfigReloadLogger {
    pub fn new(agent_dir: &Path, max_bytes: Option<u64>) -> Result<Self, regex::Error> {
        Self::with_clock(agent_dir, max_bytes, Box::new(system_now_ms))
    }
    /// `new` with an injected millisecond clock for the retry window.
    pub fn with_clock(
        agent_dir: &Path,
        max_bytes: Option<u64>,
        now_ms: LogClock,
    ) -> Result<Self, regex::Error> {
        Ok(Self {
            path: agent_dir.join("logs/config-reload.log"),
            max_bytes: max_bytes
                .filter(|value| *value > 0)
                .unwrap_or(5 * 1024 * 1024),
            retry_at: 0,
            now_ms,
            sensitive_path: SENSITIVE_PATH.clone()?,
            sensitive_text: SENSITIVE_TEXT.clone()?,
        })
    }
    fn safe_text(&self, text: &str) -> String {
        let redacted = self.sensitive_text.replace_all(text, "${1}[redacted]");
        if redacted.encode_utf16().count() <= 200 {
            return redacted.into_owned();
        }
        let mut units: usize = 0;
        let prefix: String = redacted
            .chars()
            .take_while(|character| {
                units = units.saturating_add(character.len_utf16());
                units <= 197
            })
            .collect();
        format!("{prefix}...")
    }
    fn safe_paths(&self, paths: &[String]) -> Value {
        Value::Array(
            paths
                .iter()
                .filter(|path| !self.sensitive_path.is_match(path))
                .map(|path| Value::String(self.safe_text(path)))
                .collect(),
        )
    }
    fn add_path(&self, entry: &mut Map<String, Value>, path: &str) {
        if !self.sensitive_path.is_match(path) {
            entry.insert("path".into(), self.safe_text(path).into());
        }
    }
    pub fn log(&mut self, level: LogLevel, event: LogEvent<'_>) -> ConfigReloadLogStatus {
        if (self.now_ms)() < self.retry_at {
            return ConfigReloadLogStatus {
                written: false,
                disabled: true,
            };
        }
        let mut entry = Map::new();
        entry.insert(
            "ts".into(),
            chrono::Utc::now()
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
                .into(),
        );
        entry.insert("level".into(), level.as_str().into());
        entry.insert("event".into(), event.name().into());
        match event {
            LogEvent::WatcherStarted { target_count } => {
                entry.insert("targetCount".into(), finite_number(target_count));
            }
            LogEvent::ChangeDetected {
                registration_id,
                paths,
                deferred,
            } => {
                entry.insert(
                    "registrationId".into(),
                    self.safe_text(registration_id).into(),
                );
                entry.insert("paths".into(), self.safe_paths(paths));
                entry.insert("deferred".into(), deferred.into());
            }
            LogEvent::SelfWriteSuppressed { path }
            | LogEvent::RoutineSettingsChangeSuppressed { path }
            | LogEvent::GeneratedShimChangeSuppressed { path } => self.add_path(&mut entry, path),
            LogEvent::ReloadRequested { reason, paths } => {
                entry.insert("reason".into(), self.safe_text(reason).into());
                entry.insert("paths".into(), self.safe_paths(paths));
            }
            LogEvent::ReloadDeferred { reason } => {
                entry.insert("reason".into(), self.safe_text(reason).into());
            }
            LogEvent::ReloadCompleted { duration_ms } => {
                entry.insert("durationMs".into(), finite_number(duration_ms));
            }
            LogEvent::ReloadLoopStopped { paths, reloads } => {
                entry.insert("paths".into(), self.safe_paths(paths));
                entry.insert("reloads".into(), finite_number(reloads));
            }
            LogEvent::ValidationRejected {
                registration_id,
                error_count,
            }
            | LogEvent::RegistrationRejected {
                registration_id,
                error_count,
            } => {
                entry.insert(
                    "registrationId".into(),
                    self.safe_text(registration_id).into(),
                );
                entry.insert("errorCount".into(), finite_number(error_count));
            }
            LogEvent::WatcherError { path, message } => {
                self.add_path(&mut entry, path);
                entry.insert("message".into(), self.safe_text(message).into());
            }
            LogEvent::RegistrationAdded { id } | LogEvent::RegistrationRemoved { id } => {
                entry.insert("id".into(), self.safe_text(id).into());
            }
            LogEvent::RegistrationRejectionSuppressed { .. } => {}
        }
        let text = format!(
            "{}\n",
            maho_ai::api::google_shared::js_json_stringify(&Value::Object(entry))
        );
        match self.write_line(&text) {
            Ok(()) => ConfigReloadLogStatus {
                written: true,
                disabled: false,
            },
            Err(_) => {
                self.retry_at = (self.now_ms)().saturating_add(LOG_SINK_RETRY_MS);
                ConfigReloadLogStatus {
                    written: false,
                    disabled: true,
                }
            }
        }
    }
    fn write_line(&self, text: &str) -> std::io::Result<()> {
        if let Some(parent) = self.path.parent() {
            let mut builder = fs::DirBuilder::new();
            builder.recursive(true);
            #[cfg(unix)]
            builder.mode(0o700);
            builder.create(parent)?;
        }
        let incoming = u64::try_from(text.len()).map_err(std::io::Error::other)?;
        rotate_log_if_needed(&self.path, incoming, self.max_bytes)?;
        let mut options = fs::OpenOptions::new();
        options.append(true).create(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut file = options.open(&self.path)?;
        file.write_all(text.as_bytes())?;
        #[cfg(unix)]
        fs::set_permissions(&self.path, fs::Permissions::from_mode(0o600))?;
        Ok(())
    }
}
fn finite_number(number: f64) -> Value {
    if !number.is_finite() {
        return json!(0);
    }
    if number == 0.0 {
        return json!(0);
    }
    if number.fract() == 0.0 && number.abs() <= 9_007_199_254_740_991.0 {
        return json!(number as i64);
    }
    json!(number)
}
fn exceeds_cap(path: &Path, incoming: u64, max_bytes: u64) -> std::io::Result<bool> {
    match fs::metadata(path) {
        Ok(metadata) => Ok(metadata.len().saturating_add(incoming) > max_bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}
/// `rotateLogIfNeeded`: rotates to `<file>.1` under an exclusive `.rotate-lock`, re-checking the size
/// while holding it, so concurrent appenders never move a generation another process just rotated.
/// A held lock skips rotation for this line; a lock older than ten seconds is cleared.
fn rotate_log_if_needed(path: &Path, incoming: u64, max_bytes: u64) -> std::io::Result<()> {
    if !exceeds_cap(path, incoming, max_bytes)? {
        return Ok(());
    }
    let mut lock_path = path.as_os_str().to_owned();
    lock_path.push(".rotate-lock");
    let lock_path = PathBuf::from(lock_path);
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let lock = match options.open(&lock_path) {
        Ok(lock) => lock,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            match fs::metadata(&lock_path).and_then(|metadata| metadata.modified()) {
                Ok(modified) => {
                    if modified
                        .elapsed()
                        .is_ok_and(|age| age.as_millis() > ROTATION_LOCK_STALE_MS)
                    {
                        match fs::remove_file(&lock_path) {
                            Ok(()) => {}
                            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                            Err(error) => return Err(error),
                        }
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
            return Ok(());
        }
        Err(error) => return Err(error),
    };
    let result = (|| {
        if !exceeds_cap(path, incoming, max_bytes)? {
            return Ok(());
        }
        let previous = path.with_file_name("config-reload.log.1");
        match fs::rename(path, &previous) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        }
        #[cfg(unix)]
        fs::set_permissions(previous, fs::Permissions::from_mode(0o600))?;
        Ok(())
    })();
    drop(lock);
    let removed = match fs::remove_file(&lock_path) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    };
    result.and(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    };

    fn clocked(dir: &Path) -> (ConfigReloadLogger, Arc<AtomicU64>) {
        let now = Arc::new(AtomicU64::new(1_000));
        let clock = now.clone();
        (
            ConfigReloadLogger::with_clock(
                dir,
                None,
                Box::new(move || clock.load(Ordering::SeqCst)),
            )
            .unwrap(),
            now,
        )
    }

    /// Port of regressions/2976-log-sink-recovery.test.ts (config-reload case).
    #[test]
    fn failed_sink_reopens_its_file_after_the_retry_window() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("logs"), "blocks the log directory").unwrap();
        let (mut logger, now) = clocked(root.path());
        let failed = ConfigReloadLogStatus {
            written: false,
            disabled: true,
        };
        assert_eq!(
            logger.log(
                LogLevel::Info,
                LogEvent::WatcherStarted { target_count: 1.0 }
            ),
            failed
        );
        fs::remove_file(root.path().join("logs")).unwrap();
        assert_eq!(
            logger.log(
                LogLevel::Info,
                LogEvent::WatcherStarted { target_count: 2.0 }
            ),
            failed
        );
        now.fetch_add(LOG_SINK_RETRY_MS, Ordering::SeqCst);
        assert_eq!(
            logger.log(
                LogLevel::Info,
                LogEvent::WatcherStarted { target_count: 3.0 }
            ),
            ConfigReloadLogStatus {
                written: true,
                disabled: false
            }
        );
        let text = fs::read_to_string(root.path().join("logs/config-reload.log")).unwrap();
        assert!(
            text.contains("\"targetCount\":3") && !text.contains("\"targetCount\":2"),
            "{text}"
        );
    }

    #[test]
    fn reload_loop_stopped_records_safe_paths_and_reload_count() {
        let root = tempfile::tempdir().unwrap();
        let (mut logger, _) = clocked(root.path());
        logger.log(
            LogLevel::Warn,
            LogEvent::ReloadLoopStopped {
                paths: &["/repo/settings.json".into(), "/agent/auth.json".into()],
                reloads: 3.0,
            },
        );
        let entry: Value = serde_json::from_str(
            fs::read_to_string(root.path().join("logs/config-reload.log"))
                .unwrap()
                .trim(),
        )
        .unwrap();
        assert_eq!(
            (
                entry["level"].clone(),
                entry["event"].clone(),
                entry["paths"].clone(),
                entry["reloads"].clone()
            ),
            (
                json!("warn"),
                json!("reload_loop_stopped"),
                json!(["/repo/settings.json"]),
                json!(3)
            )
        );
    }

    #[test]
    fn held_rotation_lock_skips_rotation_for_the_line() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("config-reload.log");
        fs::write(&path, "x".repeat(100)).unwrap();
        fs::write(root.path().join("config-reload.log.rotate-lock"), "").unwrap();
        rotate_log_if_needed(&path, 50, 120).unwrap();
        assert!(path.exists() && !root.path().join("config-reload.log.1").exists());
        fs::remove_file(root.path().join("config-reload.log.rotate-lock")).unwrap();
        rotate_log_if_needed(&path, 50, 120).unwrap();
        assert!(!path.exists() && root.path().join("config-reload.log.1").exists());
        assert!(!root.path().join("config-reload.log.rotate-lock").exists());
    }
}
