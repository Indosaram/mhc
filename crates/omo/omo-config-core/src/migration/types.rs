use std::fmt;

use crate::internal::posix_path::to_posix_path;
use crate::loader::paths::process_env;
use crate::loader::types::OmoConfigEnv;
use crate::writer::types::{FsError, OmoConfigEdit, OmoConfigWriteFileSystem, StdWriteFileSystem};

pub const DEFAULT_LEASE_DURATION_MS: i64 = 30_000;
pub const GUARD_LEASE_DURATION_MS: i64 = 1_000;
pub const LIVE_OWNER_STALE_LEASE_MULTIPLIER: i64 = 2;
pub const MUTATION_GUARD_RETRY_DELAYS_MS: [u64; 5] = [2, 4, 8, 16, 32];

pub type MigrationEnvironment = OmoConfigEnv;

pub trait MigrationClock {
    fn now(&self) -> i64;
}

impl<F: Fn() -> i64> MigrationClock for F {
    fn now(&self) -> i64 {
        self()
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct DefaultMigrationClock;

impl MigrationClock for DefaultMigrationClock {
    fn now(&self) -> i64 {
        jiff::Timestamp::now().as_millisecond()
    }
}

pub trait ProcessLiveness {
    fn is_alive(&self, pid: u32) -> bool;
}

impl<F: Fn(u32) -> bool> ProcessLiveness for F {
    fn is_alive(&self, pid: u32) -> bool {
        self(pid)
    }
}

pub fn default_is_process_alive(pid: u32) -> bool {
    let output = std::process::Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .output();
    match output {
        Ok(out) => {
            if out.status.success() {
                true
            } else {
                let stderr = String::from_utf8_lossy(&out.stderr);
                !stderr.contains("No such process") && !stderr.contains("ESRCH")
            }
        }
        Err(_) => false,
    }
}

pub fn default_pid() -> u32 {
    std::process::id()
}

pub trait MigrationFileSystem: OmoConfigWriteFileSystem {
    fn replace_if_contents_match(
        &self,
        path: &str,
        expected: &str,
        content: &str,
    ) -> Result<bool, FsError>;
    fn remove_if_contents_match(&self, path: &str, expected: &str) -> Result<bool, FsError>;
}

impl MigrationFileSystem for StdWriteFileSystem {
    fn replace_if_contents_match(
        &self,
        path: &str,
        expected: &str,
        content: &str,
    ) -> Result<bool, FsError> {
        let normalized = to_posix_path(path);
        if !self.exists(&normalized) {
            return Ok(false);
        }
        let current = self.read(&normalized)?;
        if current != expected {
            return Ok(false);
        }
        self.write(&normalized, content)?;
        Ok(true)
    }

    fn remove_if_contents_match(&self, path: &str, expected: &str) -> Result<bool, FsError> {
        let normalized = to_posix_path(path);
        if !self.exists(&normalized) {
            return Ok(false);
        }
        let current = self.read(&normalized)?;
        if current != expected {
            return Ok(false);
        }
        self.unlink(&normalized)?;
        Ok(true)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationSourceDescriptor {
    pub backup_path: Option<String>,
    pub path: String,
}

impl MigrationSourceDescriptor {
    pub fn new(path: impl Into<String>) -> Self {
        MigrationSourceDescriptor {
            backup_path: None,
            path: path.into(),
        }
    }

    pub fn with_backup(path: impl Into<String>, backup_path: impl Into<String>) -> Self {
        MigrationSourceDescriptor {
            backup_path: Some(backup_path.into()),
            path: path.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct LoadedMigrationSource {
    pub backup_path: Option<String>,
    pub path: String,
    pub value: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MigrationTransformResult {
    pub diagnostics: Vec<String>,
    pub document: serde_json::Value,
}

impl From<serde_json::Value> for MigrationTransformResult {
    fn from(document: serde_json::Value) -> Self {
        MigrationTransformResult {
            diagnostics: Vec::new(),
            document,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MigrationMode {
    #[default]
    Merge,
    ReplaceTarget,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MigrationBoundary {
    JournalWritten,
    TargetWritten,
    TargetRecorded,
    SourceMoved,
    SourceRecorded,
}

impl MigrationBoundary {
    pub const fn as_str(self) -> &'static str {
        match self {
            MigrationBoundary::JournalWritten => "journal-written",
            MigrationBoundary::TargetWritten => "target-written",
            MigrationBoundary::TargetRecorded => "target-recorded",
            MigrationBoundary::SourceMoved => "source-moved",
            MigrationBoundary::SourceRecorded => "source-recorded",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MigrationStatus {
    Locked,
    Migrated,
    Planned,
    Skipped,
}

impl MigrationStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            MigrationStatus::Locked => "locked",
            MigrationStatus::Migrated => "migrated",
            MigrationStatus::Planned => "planned",
            MigrationStatus::Skipped => "skipped",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MigrationBatchStatus {
    Completed,
    Locked,
}

impl MigrationBatchStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            MigrationBatchStatus::Completed => "completed",
            MigrationBatchStatus::Locked => "locked",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationBackupMove {
    pub from: String,
    pub to: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MigrationPreview {
    pub backup_moves: Vec<MigrationBackupMove>,
    pub target_path: String,
    pub transform: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MigrationRunResult {
    pub diagnostics: Vec<String>,
    pub journal_resumed: bool,
    pub preview: Option<MigrationPreview>,
    pub status: MigrationStatus,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MigrationBatchRunResult {
    pub journal_resumed: bool,
    pub results: Vec<MigrationRunResult>,
    pub status: MigrationBatchStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MigrationError {
    Validation { target_path: String, detail: String },
    Transaction(String),
    Lock(String),
    Crash(String),
    Fs(String),
}

impl fmt::Display for MigrationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MigrationError::Validation {
                target_path,
                detail,
            } => {
                write!(
                    formatter,
                    "Migration validation failed for {target_path}: {detail}"
                )
            }
            MigrationError::Transaction(message) => write!(formatter, "{message}"),
            MigrationError::Lock(message) => write!(formatter, "{message}"),
            MigrationError::Crash(message) => write!(formatter, "{message}"),
            MigrationError::Fs(message) => write!(formatter, "{message}"),
        }
    }
}

impl std::error::Error for MigrationError {}

impl From<FsError> for MigrationError {
    fn from(error: FsError) -> Self {
        MigrationError::Fs(error.message)
    }
}

impl MigrationError {
    pub fn validation(target_path: impl Into<String>, detail: impl Into<String>) -> Self {
        MigrationError::Validation {
            target_path: target_path.into(),
            detail: detail.into(),
        }
    }

    pub fn transaction(message: impl Into<String>) -> Self {
        MigrationError::Transaction(message.into())
    }

    pub fn lock(message: impl Into<String>) -> Self {
        MigrationError::Lock(message.into())
    }

    pub fn crash(message: impl Into<String>) -> Self {
        MigrationError::Crash(message.into())
    }
}

pub struct MigrationTargetWriterInput<'a> {
    pub edits: &'a [OmoConfigEdit],
    pub env: &'a MigrationEnvironment,
    pub file_system: &'a dyn MigrationFileSystem,
    pub target_path: &'a str,
}

pub type MigrationTargetWriter<'a> =
    dyn Fn(MigrationTargetWriterInput<'_>) -> Result<(), MigrationError> + 'a;

pub type MigrationTransform<'a> =
    dyn Fn(&[LoadedMigrationSource]) -> Result<MigrationTransformResult, MigrationError> + 'a;

pub type MigrationBoundaryHook<'a> = dyn Fn(MigrationBoundary) -> Result<(), MigrationError> + 'a;

pub type AfterMigrationsHook<'a> = dyn Fn(&[MigrationRunResult]) + 'a;

pub struct MigrationPlan<'a> {
    pub id: String,
    pub mode: MigrationMode,
    pub sources: Vec<MigrationSourceDescriptor>,
    pub target_path: String,
    pub transform: Box<MigrationTransform<'a>>,
}

pub struct RunMigrationOptions<'a> {
    pub clock: Option<&'a dyn MigrationClock>,
    pub env: Option<MigrationEnvironment>,
    pub file_system: Option<&'a dyn MigrationFileSystem>,
    pub id: String,
    pub is_process_alive: Option<Box<dyn Fn(u32) -> bool + 'a>>,
    pub lease_duration_ms: Option<i64>,
    pub mode: MigrationMode,
    pub on_boundary: Option<Box<MigrationBoundaryHook<'a>>>,
    pub pid: Option<u32>,
    pub sources: Vec<MigrationSourceDescriptor>,
    pub target_path: String,
    pub transform: Box<MigrationTransform<'a>>,
    pub write_target: Option<&'a MigrationTargetWriter<'a>>,
}

pub struct RunMigrationsOptions<'a> {
    pub after_migrations: Option<Box<AfterMigrationsHook<'a>>>,
    pub clock: Option<&'a dyn MigrationClock>,
    pub discover: Box<dyn Fn() -> Vec<MigrationPlan<'a>> + 'a>,
    pub dry_run: bool,
    pub env: Option<MigrationEnvironment>,
    pub file_system: Option<&'a dyn MigrationFileSystem>,
    pub is_process_alive: Option<Box<dyn Fn(u32) -> bool + 'a>>,
    pub lease_duration_ms: Option<i64>,
    pub on_boundary: Option<Box<MigrationBoundaryHook<'a>>>,
    pub pid: Option<u32>,
    pub write_target: Option<&'a MigrationTargetWriter<'a>>,
}

pub fn default_migration_env() -> MigrationEnvironment {
    process_env()
}
