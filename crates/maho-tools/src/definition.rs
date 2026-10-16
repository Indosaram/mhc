//! Non-rendering tool contracts owned by this crate, shared with extension hosts.
use std::{future::Future, path::Path, pin::Pin, sync::Arc};
use maho_ai::{model::Model, types::{ConstrainedSampling, FreeformToolFormat, ThinkingLevel}};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub type ToolFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, ToolError>> + Send + 'a>>;

#[derive(Debug, thiserror::Error)]
pub enum ToolError {
    #[error("{0}")]
    Message(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("Operation aborted")]
    Aborted,
}

/// Re-labels a filesystem error the way Node's `fs` reports it to the model:
/// `ENOENT: no such file or directory, access '/abs/path'`. The error kind is preserved.
pub fn node_fs_error(error: std::io::Error, syscall: &str, path: &Path) -> std::io::Error {
    use std::io::ErrorKind;
    let code = match (error.raw_os_error(), error.kind()) {
        (Some(1), _) if cfg!(unix) => Some(("EPERM", "operation not permitted")),
        (_, ErrorKind::NotFound) => Some(("ENOENT", "no such file or directory")),
        (_, ErrorKind::PermissionDenied) => Some(("EACCES", "permission denied")),
        (_, ErrorKind::IsADirectory) => Some(("EISDIR", "illegal operation on a directory")),
        (_, ErrorKind::NotADirectory) => Some(("ENOTDIR", "not a directory")),
        (_, ErrorKind::AlreadyExists) => Some(("EEXIST", "file already exists")),
        (_, ErrorKind::StorageFull) => Some(("ENOSPC", "no space left on device")),
        (_, ErrorKind::ReadOnlyFilesystem) => Some(("EROFS", "read-only file system")),
        (_, ErrorKind::ResourceBusy) => Some(("EBUSY", "resource busy or locked")),
        _ => None,
    };
    let message = match code {
        Some((code, description)) => format!("{code}: {description}, {syscall} '{}'", path.display()),
        None => format!("{error}, {syscall} '{}'", path.display()),
    };
    std::io::Error::new(error.kind(), message)
}

/// Read-only session surface consumed by the shell environment builder.
pub trait ToolSessionManager: Send + Sync {
    fn session_id(&self) -> &str;
    fn session_file(&self) -> Option<&Path>;
}

pub trait ToolContext: Send + Sync {
    fn cwd(&self) -> &Path;
    fn model(&self) -> Option<&Model>;
    fn thinking_level(&self) -> Option<ThinkingLevel>;
    fn session_manager(&self) -> &dyn ToolSessionManager;
    fn goal_store_file(&self) -> Option<&Path>;
    fn browser_engine(&self) -> Option<&str> { None }
    /// The session's steering abort signal, when the host exposes one (codemode detaches a
    /// running eval cell on steering). Defaults to `None` for hosts without a steering surface.
    fn get_steering_signal(&self) -> Option<AbortSignal> { None }
    /// The session's live prompt-cache-safe foreground wait budget in seconds
    /// (`getPromptCacheSafeWaitSeconds`). Defaults to `None` for hosts without a session.
    fn get_prompt_cache_safe_wait_seconds(&self) -> Option<f64> { None }
    fn take_approved_monitor_parent(
        &self, _tool_call_id: &str, _input: &Value,
    ) -> Result<Option<std::path::PathBuf>, ToolError> {
        Err(ToolError::Message("Approved monitor parent is unavailable".into()))
    }
}

/// Cloneable abort subscription; receivers observe cancellation even when registered afterwards.
#[derive(Clone, Debug)]
pub struct AbortSignal(tokio::sync::watch::Sender<bool>);
impl Default for AbortSignal {
    fn default() -> Self { Self(tokio::sync::watch::channel(false).0) }
}
impl AbortSignal {
    pub fn abort(&self) { self.0.send_replace(true); }
    pub fn is_aborted(&self) -> bool { *self.0.borrow() }
    pub fn check(&self) -> Result<(), ToolError> {
        if self.is_aborted() { Err(ToolError::Aborted) } else { Ok(()) }
    }
    pub async fn cancelled(&self) {
        let mut receiver = self.0.subscribe();
        loop {
            if *receiver.borrow_and_update() { return; }
            if receiver.changed().await.is_err() { return; }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ToolContent {
    Text { text: String, #[serde(skip_serializing_if = "Option::is_none")] audience: Option<String> },
    Image { data: String, #[serde(rename = "mimeType")] mime_type: String },
}
impl ToolContent {
    pub fn text(text: impl Into<String>) -> Self { Self::Text { text: text.into(), audience: None } }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolResult {
    pub content: Vec<ToolContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub structured_content: Option<Value>,
}
impl Default for ToolResult {
    fn default() -> Self { Self { content: Vec::new(), details: None, structured_content: None } }
}
impl ToolResult {
    pub fn text(text: impl Into<String>) -> Self { Self { content: vec![ToolContent::text(text)], details: None, structured_content: None } }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolExposure { Direct, Search, Eval }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolExecutionMode { Sequential, Parallel }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RenderShell { Default, #[serde(rename = "self")] Own }

pub type ToolUpdateCallback = Arc<dyn Fn(ToolResult) -> Result<(), ToolError> + Send + Sync>;
pub type PrepareArguments = Arc<dyn Fn(Value) -> Result<Value, ToolError> + Send + Sync>;
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct KernelPrelude {
    pub javascript: String,
    pub python: String,
    pub documentation: String,
    pub exports: Vec<String>,
    pub declarations: Option<String>,
    pub safety: Option<String>,
    pub method_allowlist: Option<Vec<String>>,
}
pub struct ToolCall<'a> {
    pub id: &'a str,
    pub params: Value,
    pub signal: AbortSignal,
    pub on_update: Option<ToolUpdateCallback>,
    pub context: Option<&'a dyn ToolContext>,
}
pub type ToolExecutor = Arc<dyn for<'a> Fn(ToolCall<'a>) -> ToolFuture<'a, ToolResult> + Send + Sync>;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolPermissionRequest {
    pub permission: String,
    pub patterns: Vec<String>,
    pub always: Vec<String>,
}
impl ToolPermissionRequest {
    pub fn new(permission: impl Into<String>, patterns: Vec<String>, always: Vec<String>) -> Self {
        Self { permission: permission.into(), patterns, always }
    }
}
pub type ToolPermissionParser = Arc<dyn Fn(&Value, &Path) -> Vec<ToolPermissionRequest> + Send + Sync>;

#[derive(Clone)]
pub struct ToolDefinition {
    pub name: String,
    pub label: String,
    pub description: String,
    pub exposure: Option<ToolExposure>,
    pub search_text: Option<String>,
    pub search_keywords: Option<Vec<String>>,
    pub search_group: Option<String>,
    pub allow_lazy_activation: Option<bool>,
    pub prompt_snippet: Option<String>,
    pub prompt_guidelines: Option<Vec<String>>,
    pub parameters: Value,
    pub output_schema: Option<Value>,
    pub freeform: Option<FreeformToolFormat>,
    pub constrained_sampling: Option<ConstrainedSampling>,
    pub render_shell: Option<RenderShell>,
    pub prepare_arguments: Option<PrepareArguments>,
    pub execution_mode: Option<ToolExecutionMode>,
    pub execute: ToolExecutor,
    pub kernel_prelude: Option<KernelPrelude>,
    pub permission_parser: Option<ToolPermissionParser>,
}
impl ToolDefinition {
    pub fn new(name: &str, description: &str, parameters: Value, execute: ToolExecutor) -> Self {
        Self { name: name.into(), label: name.into(), description: description.into(), parameters, execute,
            exposure: None, search_text: None, search_keywords: None, search_group: None,
            allow_lazy_activation: None, prompt_snippet: None, prompt_guidelines: None,
            freeform: None, constrained_sampling: None, render_shell: None,
            prepare_arguments: None, execution_mode: None,
            permission_parser: None, kernel_prelude: None, output_schema: None }
    }
}
