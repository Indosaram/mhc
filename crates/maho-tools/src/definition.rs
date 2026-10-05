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
    /// The session's steering abort signal, when the host exposes one (codemode detaches a
    /// running eval cell on steering). Defaults to `None` for hosts without a steering surface.
    fn get_steering_signal(&self) -> Option<AbortSignal> { None }
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
}
impl ToolResult {
    pub fn text(text: impl Into<String>) -> Self { Self { content: vec![ToolContent::text(text)], details: None } }
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
pub struct ToolCall<'a> {
    pub id: &'a str,
    pub params: Value,
    pub signal: AbortSignal,
    pub on_update: Option<ToolUpdateCallback>,
    pub context: Option<&'a dyn ToolContext>,
}
pub type ToolExecutor = Arc<dyn for<'a> Fn(ToolCall<'a>) -> ToolFuture<'a, ToolResult> + Send + Sync>;

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
    pub freeform: Option<FreeformToolFormat>,
    pub constrained_sampling: Option<ConstrainedSampling>,
    pub render_shell: Option<RenderShell>,
    pub prepare_arguments: Option<PrepareArguments>,
    pub execution_mode: Option<ToolExecutionMode>,
    pub execute: ToolExecutor,
}
impl ToolDefinition {
    pub fn new(name: &str, description: &str, parameters: Value, execute: ToolExecutor) -> Self {
        Self { name: name.into(), label: name.into(), description: description.into(), parameters, execute,
            exposure: None, search_text: None, search_keywords: None, search_group: None,
            allow_lazy_activation: None, prompt_snippet: None, prompt_guidelines: None,
            freeform: None, constrained_sampling: None, render_shell: None,
            prepare_arguments: None, execution_mode: None }
    }
}
