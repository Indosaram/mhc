//! Port of senpi packages/agent/src/harness/types.ts.

use std::fmt;
use std::sync::Arc;

use maho_ai::model::Model;
use maho_ai::types::{BoxFuture, CacheRetention, DeferredOption, Tool, Transport};
use serde_json::{Map, Value};

use super::context::{Context, ContextKey};
use super::result::Result;
use crate::harness::session::types::JsonValue;
use crate::types::{AgentToolResult, ReplayPolicy};

/// `getOrUndefined(value)`: normalize nullable values to optional values.
pub fn get_or_undefined<T>(value: Option<T>) -> Option<T> {
    value
}

/// `toError(error)`: normalize unknown thrown values into an error with a message.
pub fn to_error(error: UnknownError) -> String {
    error.message
}

/// Type-erased thrown value. TS catches `unknown`; Rust carries the same as a message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownError {
    pub message: String,
}

impl UnknownError {
    pub fn new(message: impl Into<String>) -> Self {
        Self { message: message.into() }
    }
}

impl fmt::Display for UnknownError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

/// `Skill` loaded from a `SKILL.md` file or provided by an application.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub content: String,
    pub file_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disable_model_invocation: Option<bool>,
}

/// `PromptTemplate` that can be formatted into a prompt for explicit invocation.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptTemplate {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub content: String,
}

/// `AgentHarnessResources`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentHarnessResources {
    pub prompt_templates: Option<Vec<PromptTemplate>>,
    pub skills: Option<Vec<Skill>>,
}

/// `AgentHarnessToolUpdateOptions`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AgentHarnessToolUpdateOptions {
    pub checkpoint: Option<bool>,
}

/// `AgentHarnessToolUpdateCallback<TDetails>`.
pub type AgentHarnessToolUpdateCallback =
    Arc<dyn Fn(AgentToolResult, Option<AgentHarnessToolUpdateOptions>) + Send + Sync>;

/// `AgentHarnessToolInvocation`: stable harness identity for one logical tool call.
pub trait AgentHarnessToolInvocation: Send + Sync {
    fn invocation_id(&self) -> &str;
    fn operation_id(&self) -> &str;
    fn turn_id(&self) -> &str;
    fn get_memo<'a>(&'a self, name: &'a str) -> BoxFuture<'a, Option<JsonValue>>;
    fn set_memo<'a>(&'a self, name: &'a str, value: Option<JsonValue>) -> BoxFuture<'a, ()>;
}

/// `AgentHarnessTool.execute(...)`.
pub type AgentHarnessToolExecute<TContext> = Arc<
    dyn Fn(
            String,
            Value,
            AgentHarnessToolUpdateCallback,
            TContext,
            Arc<dyn AgentHarnessToolInvocation>,
            Context,
        ) -> BoxFuture<'static, Result<AgentToolResult, String>>
        + Send
        + Sync,
>;

/// `AgentHarnessTool<TContext, TParameters, TDetails>`.
#[derive(Clone)]
pub struct AgentHarnessTool<TContext> {
    pub label: String,
    pub prepare_arguments: Option<Arc<dyn Fn(Value) -> Value + Send + Sync>>,
    pub execute: AgentHarnessToolExecute<TContext>,
    pub replay: Option<ReplayPolicy>,
    pub tool: Tool,
}

impl<TContext> AgentHarnessTool<TContext> {
    pub fn name(&self) -> &str {
        &self.tool.name
    }
}

impl<TContext> fmt::Debug for AgentHarnessTool<TContext> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AgentHarnessTool").field("label", &self.label).finish_non_exhaustive()
    }
}

/// `AgentHarnessToolContextSource<TContext>`: a static context or a provider resolved per turn.
pub enum AgentHarnessToolContextSource<TContext> {
    Static(TContext),
    Provider(Arc<dyn Fn(Context) -> BoxFuture<'static, TContext> + Send + Sync>),
}

/// `AgentHarnessStreamOptions`.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentHarnessStreamOptions {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport: Option<Transport>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_retries: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_retry_delay_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headers: Option<Map<String, Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Map<String, Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_retention: Option<CacheRetention>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deferred: Option<DeferredOption>,
}

/// `AgentHarnessStreamOptionsPatch`: `undefined` values delete keys.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AgentHarnessStreamOptionsPatch {
    pub transport: Option<Option<Transport>>,
    pub timeout_ms: Option<Option<u64>>,
    pub max_retries: Option<Option<u32>>,
    pub max_retry_delay_ms: Option<Option<u64>>,
    pub cache_retention: Option<Option<CacheRetention>>,
    pub deferred: Option<Option<DeferredOption>>,
    pub headers: Option<Option<std::collections::BTreeMap<String, Option<String>>>>,
    pub metadata: Option<Option<std::collections::BTreeMap<String, Option<Value>>>>,
}

/// `FileKind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FileKind {
    File,
    Directory,
    Symlink,
}

impl FileKind {
    pub fn as_str(self) -> &'static str {
        match self {
            FileKind::File => "file",
            FileKind::Directory => "directory",
            FileKind::Symlink => "symlink",
        }
    }
}

/// `FileErrorCode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileErrorCode {
    Aborted,
    NotFound,
    PermissionDenied,
    NotDirectory,
    IsDirectory,
    Invalid,
    NotSupported,
    Unknown,
}

impl FileErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            FileErrorCode::Aborted => "aborted",
            FileErrorCode::NotFound => "not_found",
            FileErrorCode::PermissionDenied => "permission_denied",
            FileErrorCode::NotDirectory => "not_directory",
            FileErrorCode::IsDirectory => "is_directory",
            FileErrorCode::Invalid => "invalid",
            FileErrorCode::NotSupported => "not_supported",
            FileErrorCode::Unknown => "unknown",
        }
    }
}

/// `FileError`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileError {
    pub code: FileErrorCode,
    pub message: String,
    pub path: Option<String>,
    pub cause: Option<String>,
}

impl FileError {
    pub fn new(code: FileErrorCode, message: impl Into<String>, path: Option<String>) -> Self {
        Self { code, message: message.into(), path, cause: None }
    }

    pub fn with_cause(mut self, cause: impl Into<String>) -> Self {
        self.cause = Some(cause.into());
        self
    }
}

impl fmt::Display for FileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for FileError {}

/// `Result<T, FileError>`: every `FileSystem` operation encodes failure this way.
pub type FileResult<T> = Result<T, FileError>;

/// Future returned by a `FileSystem` operation.
pub type FileFuture<'a, T> = BoxFuture<'a, T>;

/// `ExecutionErrorCode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionErrorCode {
    Aborted,
    Timeout,
    ShellUnavailable,
    SpawnError,
    CallbackError,
    Unknown,
}

impl ExecutionErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            ExecutionErrorCode::Aborted => "aborted",
            ExecutionErrorCode::Timeout => "timeout",
            ExecutionErrorCode::ShellUnavailable => "shell_unavailable",
            ExecutionErrorCode::SpawnError => "spawn_error",
            ExecutionErrorCode::CallbackError => "callback_error",
            ExecutionErrorCode::Unknown => "unknown",
        }
    }
}

/// `ExecutionError`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionError {
    pub code: ExecutionErrorCode,
    pub message: String,
    pub cause: Option<String>,
}

impl ExecutionError {
    pub fn new(code: ExecutionErrorCode, message: impl Into<String>) -> Self {
        Self { code, message: message.into(), cause: None }
    }
}

impl fmt::Display for ExecutionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ExecutionError {}

/// `CompactionErrorCode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactionErrorCode {
    Aborted,
    SummarizationFailed,
}

/// `CompactionError`.
#[derive(Debug, Clone)]
pub struct CompactionError {
    pub code: CompactionErrorCode,
    pub message: String,
    pub cause: Option<String>,
}

impl fmt::Display for CompactionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for CompactionError {}

/// `BranchSummaryErrorCode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BranchSummaryErrorCode {
    Aborted,
    SummarizationFailed,
}

/// `BranchSummaryError`.
#[derive(Debug, Clone)]
pub struct BranchSummaryError {
    pub code: BranchSummaryErrorCode,
    pub message: String,
    pub cause: Option<String>,
}

impl fmt::Display for BranchSummaryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for BranchSummaryError {}

/// `FileInfo`.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileInfo {
    pub name: String,
    pub path: String,
    pub kind: FileKind,
    pub size: u64,
    pub mtime_ms: f64,
}

/// One UTF-8 line read from a text file.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TextLine {
    pub text: String,
    pub terminated: bool,
}

/// `TextLineReader`.
pub trait TextLineReader: Send + Sync {
    /// `readLine(context)`.
    fn read_line<'a>(&'a self, context: &'a Context) -> BoxFuture<'a, Result<Option<TextLine>, FileError>>;

    /// `close(context)`. Must be best-effort and must not throw.
    fn close<'a>(&'a self, context: &'a Context) -> BoxFuture<'a, ()>;
}

/// `ShellOutputRetention`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ShellOutputRetention {
    Head,
    #[default]
    Tail,
}

/// `ShellOutputLimits`.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShellOutputLimits {
    pub max_bytes: u64,
    pub max_lines: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retain: Option<ShellOutputRetention>,
}

/// `ShellOutputCaptureOptions`.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ShellOutputCaptureOptions {
    pub limits: ShellOutputLimits,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spill: Option<bool>,
}

/// `ShellOutputTruncation`: `Omit<TruncationResult, "content">`.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShellOutputTruncation {
    pub truncated: bool,
    pub truncated_by: Option<TruncationLimit>,
    pub total_lines: u64,
    pub total_bytes: u64,
    pub output_lines: u64,
    pub output_bytes: u64,
    pub last_line_partial: bool,
    pub first_line_exceeds_limit: bool,
    pub max_lines: u64,
    pub max_bytes: u64,
}

/// Which limit was hit: `"lines" | "bytes" | null`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TruncationLimit {
    Lines,
    Bytes,
}

/// `ShellOutputMetadata`.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShellOutputMetadata {
    pub truncation: ShellOutputTruncation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spill_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_line_bytes: Option<u64>,
}

/// `ShellOutputView`.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShellOutputView {
    pub text: String,
    pub truncation: ShellOutputTruncation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spill_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_line_bytes: Option<u64>,
}

/// `ShellOutputUpdate`.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum ShellOutputUpdate {
    Replace { output: ShellOutputView },
    Append { text: String, metadata: ShellOutputMetadata },
    Slide { drop: u64, text: String, metadata: ShellOutputMetadata },
    Metadata { metadata: ShellOutputMetadata },
}

/// `ShellExecResult`.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ShellExecResult {
    pub exit_code: i32,
    pub truncation: ShellOutputTruncation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spill_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_line_bytes: Option<u64>,
}

/// `ShellExecOptions.onUpdate`.
pub type ShellOutputUpdateHandler =
    Arc<dyn Fn(ShellOutputUpdate, Context) -> BoxFuture<'static, ()> + Send + Sync>;

/// `ShellExecOptions`.
#[derive(Default)]
pub struct ShellExecOptions {
    pub cwd: Option<String>,
    pub env: Option<Map<String, Value>>,
    pub inherit_env: Option<bool>,
    /// Timeout in seconds.
    pub timeout: Option<f64>,
    pub capture: Option<ShellOutputCaptureOptions>,
    pub on_update: Option<ShellOutputUpdateHandler>,
}

impl Clone for ShellExecOptions {
    fn clone(&self) -> Self {
        Self {
            cwd: self.cwd.clone(),
            env: self.env.clone(),
            inherit_env: self.inherit_env,
            timeout: self.timeout,
            capture: self.capture.clone(),
            on_update: self.on_update.clone(),
        }
    }
}

impl fmt::Debug for ShellExecOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ShellExecOptions")
            .field("cwd", &self.cwd)
            .field("timeout", &self.timeout)
            .finish_non_exhaustive()
    }
}

/// `FileSystem` capability used by the harness. Operations must never throw; failures are
/// encoded in the returned `Result`.
pub trait FileSystem: Send + Sync {
    fn cwd(&self) -> &str;
    fn absolute_path<'a>(&'a self, path: &'a str, context: &'a Context) -> BoxFuture<'a, Result<String, FileError>>;
    fn join_path<'a>(&'a self, parts: Vec<String>, context: &'a Context) -> BoxFuture<'a, Result<String, FileError>>;
    fn read_text_file<'a>(&'a self, path: &'a str, context: &'a Context) -> BoxFuture<'a, Result<String, FileError>>;
    fn open_text_line_reader<'a>(
        &'a self,
        path: &'a str,
        context: &'a Context,
    ) -> BoxFuture<'a, Result<Box<dyn TextLineReader>, FileError>>;
    fn read_text_lines<'a>(
        &'a self,
        path: &'a str,
        max_lines: Option<u64>,
        context: &'a Context,
    ) -> BoxFuture<'a, Result<Vec<String>, FileError>>;
    fn read_binary_file<'a>(&'a self, path: &'a str, context: &'a Context) -> BoxFuture<'a, Result<Vec<u8>, FileError>>;
    fn write_file<'a>(
        &'a self,
        path: &'a str,
        content: &'a [u8],
        context: &'a Context,
    ) -> BoxFuture<'a, Result<(), FileError>>;
    fn append_file<'a>(
        &'a self,
        path: &'a str,
        content: &'a [u8],
        context: &'a Context,
    ) -> BoxFuture<'a, Result<(), FileError>>;
    fn rename_file<'a>(
        &'a self,
        source_path: &'a str,
        destination_path: &'a str,
        context: &'a Context,
    ) -> BoxFuture<'a, Result<(), FileError>>;
    fn file_info<'a>(&'a self, path: &'a str, context: &'a Context) -> BoxFuture<'a, Result<FileInfo, FileError>>;
    fn list_dir<'a>(&'a self, path: &'a str, context: &'a Context) -> BoxFuture<'a, Result<Vec<FileInfo>, FileError>>;
    fn canonical_path<'a>(&'a self, path: &'a str, context: &'a Context) -> BoxFuture<'a, Result<String, FileError>>;
    fn exists<'a>(&'a self, path: &'a str, context: &'a Context) -> BoxFuture<'a, Result<bool, FileError>>;
    fn create_dir<'a>(
        &'a self,
        path: &'a str,
        recursive: Option<bool>,
        context: &'a Context,
    ) -> BoxFuture<'a, Result<(), FileError>>;
    fn remove<'a>(
        &'a self,
        path: &'a str,
        recursive: Option<bool>,
        force: Option<bool>,
        context: &'a Context,
    ) -> BoxFuture<'a, Result<(), FileError>>;
    fn create_temp_dir<'a>(
        &'a self,
        prefix: Option<String>,
        context: &'a Context,
    ) -> BoxFuture<'a, Result<String, FileError>>;
    fn create_temp_file<'a>(
        &'a self,
        prefix: Option<String>,
        suffix: Option<String>,
        context: &'a Context,
    ) -> BoxFuture<'a, Result<String, FileError>>;
    fn cleanup<'a>(&'a self, context: &'a Context) -> BoxFuture<'a, ()>;
}

/// `Shell` execution capability used by the harness.
pub trait Shell: Send + Sync {
    fn exec<'a>(
        &'a self,
        command: &'a str,
        options: Option<ShellExecOptions>,
        context: &'a Context,
    ) -> BoxFuture<'a, Result<ShellExecResult, ExecutionError>>;
    fn cleanup<'a>(&'a self, context: &'a Context) -> BoxFuture<'a, ()>;
}

/// `ExecutionEnv extends FileSystem, Shell`.
pub trait ExecutionEnv: FileSystem + Shell {}

impl<T: FileSystem + Shell> ExecutionEnv for T {}

/// The harness tool's provider-facing model (`AgentHarnessOptions.model`).
pub type HarnessModel = Model;

/// `ContextKey` re-export so callers need only this module for tool wiring.
pub use super::context::ContextKey as HarnessContextKey;

/// Keep `ContextKey` referenced from this module's public surface.
pub type AnyContextKey<T> = ContextKey<T>;
