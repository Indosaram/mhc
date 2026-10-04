//! Structured component logger (upstream `ComponentLogger`, `omo-senpi/src/extension/types.ts:40`).

use std::sync::{Arc, Mutex, PoisonError};

use maho_ext_api::{ComponentLogger, JsonValue};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogLevel {
    Info,
    Warn,
    Error,
}

impl LogLevel {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
        }
    }
}

pub type LogSink = Arc<dyn Fn(LogLevel, &str, Option<&JsonValue>) + Send + Sync>;

/// Upstream `compose.ts` `defaultLogger`: `console.info/warn/error`.
#[derive(Clone, Copy, Debug, Default)]
pub struct StderrLogger;

impl ComponentLogger for StderrLogger {
    fn info(&self, message: &str, details: Option<&JsonValue>) {
        write(LogLevel::Info, message, details);
    }

    fn warn(&self, message: &str, details: Option<&JsonValue>) {
        write(LogLevel::Warn, message, details);
    }

    fn error(&self, message: &str, details: Option<&JsonValue>) {
        write(LogLevel::Error, message, details);
    }
}

fn write(level: LogLevel, message: &str, details: Option<&JsonValue>) {
    match details {
        Some(details) => eprintln!("maho-omo {}: {message} {details}", level.as_str()),
        None => eprintln!("maho-omo {}: {message}", level.as_str()),
    }
}

/// Generalizes lane-42's `ConfigWatchLogSink`: routes every level through one host sink.
#[derive(Clone)]
pub struct SinkLogger {
    sink: LogSink,
}

impl SinkLogger {
    pub fn new(sink: LogSink) -> Self {
        Self { sink }
    }

    pub fn sink(&self) -> LogSink {
        Arc::clone(&self.sink)
    }
}

impl ComponentLogger for SinkLogger {
    fn info(&self, message: &str, details: Option<&JsonValue>) {
        (self.sink)(LogLevel::Info, message, details);
    }

    fn warn(&self, message: &str, details: Option<&JsonValue>) {
        (self.sink)(LogLevel::Warn, message, details);
    }

    fn error(&self, message: &str, details: Option<&JsonValue>) {
        (self.sink)(LogLevel::Error, message, details);
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct LogEntry {
    pub level: LogLevel,
    pub message: String,
    pub details: Option<JsonValue>,
}

#[derive(Clone, Default)]
pub struct RecordingLogger {
    entries: Arc<Mutex<Vec<LogEntry>>>,
}

impl RecordingLogger {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn entries(&self) -> Vec<LogEntry> {
        self.entries.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }
}

impl ComponentLogger for RecordingLogger {
    fn info(&self, message: &str, details: Option<&JsonValue>) {
        self.push(LogLevel::Info, message, details);
    }

    fn warn(&self, message: &str, details: Option<&JsonValue>) {
        self.push(LogLevel::Warn, message, details);
    }

    fn error(&self, message: &str, details: Option<&JsonValue>) {
        self.push(LogLevel::Error, message, details);
    }
}

impl RecordingLogger {
    fn push(&self, level: LogLevel, message: &str, details: Option<&JsonValue>) {
        self.entries.lock().unwrap_or_else(PoisonError::into_inner).push(LogEntry {
            level,
            message: message.to_owned(),
            details: details.cloned(),
        });
    }
}
