//! Shared data types (port of `src/types.ts`).

use std::collections::BTreeMap;
use std::future::Future;
use std::io;
use std::pin::Pin;

use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use tokio::io::AsyncRead;

/// One file edit handed to the comment checker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckerEdit {
    pub file_path: String,
    pub before: String,
    pub after: String,
}

/// One file entry read from apply-patch tool metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyPatchFileMetadata {
    pub file_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub move_path: Option<String>,
    pub before: String,
    pub after: String,
    #[serde(default, rename = "type", skip_serializing_if = "Option::is_none")]
    pub r#type: Option<String>,
}

/// Operation named by an apply-patch file header.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ApplyPatchOperation {
    Add,
    Update,
    Delete,
}

/// Mutable per-file state while parsing an apply-patch body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyPatchAccumulator {
    pub operation: ApplyPatchOperation,
    pub file_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub move_path: Option<String>,
    pub old_lines: Vec<String>,
    pub new_lines: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CommentType {
    Line,
    Block,
    Docstring,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommentInfo {
    pub text: String,
    pub line_number: u64,
    pub file_path: String,
    pub comment_type: CommentType,
    pub is_docstring: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<BTreeMap<String, String>>,
}

/// One `old_string` / `new_string` pair of a multi-edit call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditPair {
    pub old_string: String,
    pub new_string: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PendingCallTool {
    Write,
    Edit,
    Multiedit,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingCall {
    pub file_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_string: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_string: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edits: Option<Vec<EditPair>>,
    pub tool: PendingCallTool,
    #[serde(rename = "sessionID")]
    pub session_id: String,
    pub timestamp: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileComments {
    pub file_path: String,
    pub comments: Vec<CommentInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FilterResult {
    pub should_skip: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

pub type CommentFilter = dyn Fn(&CommentInfo) -> FilterResult + Send + Sync;

/// `tool_input` of [`HookInput`]; absent fields are omitted from the JSON.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookToolInput {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_string: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_string: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edits: Option<Vec<EditPair>>,
}

/// JSON document piped to the comment-checker binary on stdin.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HookInput {
    pub session_id: String,
    pub tool_name: String,
    pub transcript_path: String,
    pub cwd: String,
    pub hook_event_name: String,
    pub tool_input: HookToolInput,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_response: Option<Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckResult {
    pub has_comments: bool,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SpawnSignal {
    #[serde(rename = "SIGTERM")]
    Sigterm,
    #[serde(rename = "SIGKILL")]
    Sigkill,
}

/// A readable output stream of a spawned process.
pub type ByteStream = Pin<Box<dyn AsyncRead + Send>>;

/// Future resolving to the process exit code.
pub type ExitFuture<'a> = Pin<Box<dyn Future<Output = io::Result<i32>> + Send + 'a>>;

/// Injected process handle (the TS `SpawnProcess` shape).
pub trait SpawnProcess: Send {
    fn write_stdin(&mut self, input: &str) -> io::Result<()>;
    fn end_stdin(&mut self) -> io::Result<()>;
    fn take_stdout(&mut self) -> ByteStream;
    fn take_stderr(&mut self) -> ByteStream;
    fn exited(&mut self) -> ExitFuture<'_>;
    fn kill(&mut self, signal: SpawnSignal) -> io::Result<()>;
}

/// Injected spawner: receives `[binary, "check", ...]`.
pub type SpawnFn = dyn Fn(&[String]) -> io::Result<Box<dyn SpawnProcess>> + Send + Sync;

pub type ExistsFn = dyn Fn(&str) -> bool + Send + Sync;

pub struct ResolveCommentCheckerBinaryInput<'a> {
    pub binary_name: &'a str,
    pub cached_binary_path: Option<&'a str>,
    pub exists_sync: &'a ExistsFn,
    /// `import.meta.url` of the caller: a `file://` URL or an absolute path.
    pub import_meta_url: Option<&'a str>,
    /// Defaults to `@code-yeongyu/comment-checker`.
    pub package_name: Option<&'a str>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RunCommentCheckerInput {
    pub hook_input: HookInput,
    pub binary_path: Option<String>,
    pub custom_prompt: Option<String>,
}

pub struct RunCommentCheckerOptions<'a> {
    pub spawn: &'a SpawnFn,
    pub exists_sync: &'a ExistsFn,
    /// Defaults to 30 000 ms.
    pub timeout_ms: Option<u64>,
    /// Defaults to 1 000 ms. Kept for surface parity: as in TS, the SIGKILL
    /// grace timer is cleared as soon as the timeout result is returned, so
    /// only SIGTERM is ever delivered.
    pub kill_grace_ms: Option<u64>,
}
