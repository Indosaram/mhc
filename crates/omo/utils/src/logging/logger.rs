use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::Duration;

use chrono::{SecondsFormat, Utc};
use serde::Serialize;
use serde_json::Value;

pub const DEFAULT_MAX_LOG_FILE_SIZE_BYTES: u64 = 50 * 1024 * 1024;
pub const DEFAULT_MAX_LOG_FILE_BACKUPS: u32 = 2;
pub const DEFAULT_LOG_FLUSH_INTERVAL_MS: u64 = 500;
pub const DEFAULT_LOG_BUFFER_SIZE_LIMIT: usize = 50;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LoggerTestOverrides {
    pub file_path: Option<PathBuf>,
    pub max_size_bytes: Option<u64>,
    pub max_backups: Option<u32>,
}

pub type LogPathResolver = Box<dyn Fn(&str) -> PathBuf>;

pub struct LoggerOptions {
    pub log_file_name: String,
    pub max_size_bytes: Option<u64>,
    pub max_backups: Option<u32>,
    pub flush_interval_ms: Option<u64>,
    pub buffer_size_limit: Option<usize>,
    /// Maps the log file name to its path; defaults to the OS temp dir.
    pub resolve_log_file_path: Option<LogPathResolver>,
}

impl LoggerOptions {
    pub fn new(log_file_name: impl Into<String>) -> Self {
        Self {
            log_file_name: log_file_name.into(),
            max_size_bytes: None,
            max_backups: None,
            flush_interval_ms: None,
            buffer_size_limit: None,
            resolve_log_file_path: None,
        }
    }
}

struct LoggerState {
    log_file: PathBuf,
    max_size_bytes: u64,
    max_backups: u32,
    buffer: Vec<String>,
    flush_scheduled: bool,
    generation: u64,
}

struct LoggerConfig {
    initial_log_file: PathBuf,
    default_max_size_bytes: u64,
    default_max_backups: u32,
    flush_interval: Duration,
    buffer_size_limit: usize,
}

/// A logger bound to one product log file. Cloning shares the same buffer.
#[derive(Clone)]
pub struct BoundLogger {
    config: Arc<LoggerConfig>,
    state: Arc<Mutex<LoggerState>>,
}

pub fn create_logger(options: LoggerOptions) -> BoundLogger {
    let initial_log_file = match &options.resolve_log_file_path {
        Some(resolve) => resolve(&options.log_file_name),
        None => std::env::temp_dir().join(&options.log_file_name),
    };
    let config = LoggerConfig {
        initial_log_file: initial_log_file.clone(),
        default_max_size_bytes: options
            .max_size_bytes
            .unwrap_or(DEFAULT_MAX_LOG_FILE_SIZE_BYTES),
        default_max_backups: options.max_backups.unwrap_or(DEFAULT_MAX_LOG_FILE_BACKUPS),
        flush_interval: Duration::from_millis(
            options
                .flush_interval_ms
                .unwrap_or(DEFAULT_LOG_FLUSH_INTERVAL_MS),
        ),
        buffer_size_limit: options
            .buffer_size_limit
            .unwrap_or(DEFAULT_LOG_BUFFER_SIZE_LIMIT),
    };
    let state = LoggerState {
        log_file: initial_log_file,
        max_size_bytes: config.default_max_size_bytes,
        max_backups: config.default_max_backups,
        buffer: Vec::new(),
        flush_scheduled: false,
        generation: 0,
    };
    BoundLogger {
        config: Arc::new(config),
        state: Arc::new(Mutex::new(state)),
    }
}

fn is_falsy(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::Bool(flag) => !flag,
        Value::Number(number) => number.as_f64().is_some_and(|n| n == 0.0),
        Value::String(text) => text.is_empty(),
        Value::Array(_) | Value::Object(_) => false,
    }
}

fn rotate_if_needed(state: &LoggerState) {
    let Ok(meta) = fs::metadata(&state.log_file) else {
        return;
    };
    if meta.len() <= state.max_size_bytes {
        return;
    }
    let numbered = |index: u32| PathBuf::from(format!("{}.{index}", state.log_file.display()));
    let _ = fs::remove_file(numbered(state.max_backups));
    for index in (1..state.max_backups).rev() {
        let source = numbered(index);
        if source.exists() {
            let _ = fs::rename(&source, numbered(index + 1));
        }
    }
    let _ = fs::rename(&state.log_file, numbered(1));
}

fn flush_locked(state: &mut LoggerState) {
    if state.buffer.is_empty() {
        return;
    }
    let data = state.buffer.concat();
    state.buffer.clear();
    let appended = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&state.log_file)
        .and_then(|mut file| file.write_all(data.as_bytes()));
    if appended.is_ok() {
        rotate_if_needed(state);
    }
}

impl BoundLogger {
    fn lock(&self) -> MutexGuard<'_, LoggerState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Buffer one line; serialization failures are swallowed and write nothing.
    pub fn log<T: Serialize + ?Sized>(&self, message: &str, data: Option<&T>) {
        let rendered = match data.map(serde_json::to_value) {
            None => String::new(),
            Some(Err(_)) => return,
            Some(Ok(value)) if is_falsy(&value) => String::new(),
            Some(Ok(value)) => value.to_string(),
        };
        let timestamp = Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true);
        let entry = format!("[{timestamp}] {message} {rendered}\n");
        let mut state = self.lock();
        state.buffer.push(entry);
        if state.buffer.len() >= self.config.buffer_size_limit {
            flush_locked(&mut state);
        } else if !state.flush_scheduled {
            state.flush_scheduled = true;
            let generation = state.generation;
            let shared = Arc::clone(&self.state);
            let interval = self.config.flush_interval;
            thread::spawn(move || {
                thread::sleep(interval);
                let mut state = shared.lock().unwrap_or_else(PoisonError::into_inner);
                if state.generation == generation && state.flush_scheduled {
                    state.flush_scheduled = false;
                    flush_locked(&mut state);
                }
            });
        }
    }

    pub fn get_log_file_path(&self) -> PathBuf {
        self.lock().log_file.clone()
    }

    fn cancel_scheduled(state: &mut LoggerState) {
        state.flush_scheduled = false;
        state.generation += 1;
    }

    pub fn set_logger_for_testing(&self, overrides: &LoggerTestOverrides) {
        let mut state = self.lock();
        state.buffer.clear();
        Self::cancel_scheduled(&mut state);
        if let Some(path) = &overrides.file_path {
            state.log_file.clone_from(path);
        }
        if let Some(size) = overrides.max_size_bytes {
            state.max_size_bytes = size;
        }
        if let Some(backups) = overrides.max_backups {
            state.max_backups = backups;
        }
    }

    pub fn reset_logger_for_testing(&self) {
        let mut state = self.lock();
        state.log_file.clone_from(&self.config.initial_log_file);
        state.max_size_bytes = self.config.default_max_size_bytes;
        state.max_backups = self.config.default_max_backups;
        state.buffer.clear();
        Self::cancel_scheduled(&mut state);
    }

    pub fn flush_for_testing(&self) {
        let mut state = self.lock();
        Self::cancel_scheduled(&mut state);
        flush_locked(&mut state);
    }

    pub fn log_file_exists(&self) -> bool {
        Path::new(&self.get_log_file_path()).exists()
    }
}
