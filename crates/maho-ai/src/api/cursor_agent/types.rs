//! Port of senpi packages/ai/src/api/cursor-agent/types.ts.
// ported by todo 12

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::types::{StreamOptions, ThinkingSelection, ToolResultMessage};

use super::r#gen::agent_pb::{
    DeleteArgs, DeleteResult, DiagnosticsArgs, DiagnosticsResult, GrepArgs, GrepResult, LsArgs, LsResult, McpResult,
    PiBashExecArgs, PiBashExecResult, PiEditExecArgs, PiEditExecResult, PiFindExecArgs, PiFindExecResult,
    PiGrepExecArgs, PiGrepExecResult, PiLsExecArgs, PiLsExecResult, PiReadExecArgs, PiReadExecResult,
    PiWriteExecArgs, PiWriteExecResult, ReadArgs, ReadResult, ShellArgs, ShellResult, WriteArgs, WriteResult,
};

/// The three return forms an exec handler may use: a wire result with an
/// optional transcript pairing, a bare wire result, or a bare
/// `ToolResultMessage` from which the wire result is derived.
pub enum CursorExecHandlerResult<T> {
    WithToolResult { result: T, tool_result: Option<ToolResultMessage> },
    Result(T),
    ToolResult(ToolResultMessage),
}

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Optional rewrite of a Cursor exec-channel tool result.
/// Returning `None` keeps the original result.
pub type CursorToolResultHandler =
    Arc<dyn Fn(ToolResultMessage) -> BoxFuture<'static, Option<ToolResultMessage>> + Send + Sync>;

/// Identifies the synthesized assistant block a Cursor exec call was filed
/// under, so paths that produce no handler `toolResult` can still pair one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorExecPairing {
    pub tool_call_id: String,
    pub tool_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorMcpCall {
    pub name: String,
    pub provider_identifier: String,
    pub tool_name: String,
    pub tool_call_id: String,
    pub args: serde_json::Map<String, serde_json::Value>,
    pub raw_args: std::collections::BTreeMap<String, Vec<u8>>,
    /// The frame asks only whether this call would be permitted -- it must not
    /// run. The server sends it to resolve a smart-mode approval decision
    /// ahead of the real invocation; executing here would fire a
    /// side-effecting tool the user has not yet been asked about (and fire it
    /// twice once the real call arrives).
    pub approval_only: Option<bool>,
}

pub trait CursorShellStreamCallbacks: Send {
    fn on_stdout(&mut self, data: &str);
    fn on_stderr(&mut self, data: &str);
}

/// A modern Pi exec frame plus the call id the dispatcher minted for it.
///
/// Unlike the legacy exec args (`ReadArgs`, `ShellArgs`, ...), the Pi frames
/// carry no `tool_call_id` field: the id rides the streamed `ToolCall`
/// envelope instead. The exec channel has no access to that envelope, so the
/// dispatcher mints an id and hands it to the handler, keeping the
/// synthesized transcript block and its paired `toolResult` on the same key.
pub struct CursorPiCall<TArgs> {
    pub args: TArgs,
    pub tool_call_id: String,
}

pub type ExecFuture<T> = BoxFuture<'static, CursorExecHandlerResult<T>>;

/// Each handler is an `Arc<dyn Fn>` over the generated protobuf shapes; the
/// resulting nested generic is inherent to the ported contract, so the
/// complexity lint is allowed for the whole handler table rather than by
/// rewriting every field into a bespoke alias.
#[allow(clippy::type_complexity)]
#[derive(Default)]
pub struct CursorExecHandlers {
    pub read: Option<Arc<dyn Fn(ReadArgs) -> ExecFuture<ReadResult> + Send + Sync>>,
    pub ls: Option<Arc<dyn Fn(LsArgs) -> ExecFuture<LsResult> + Send + Sync>>,
    pub grep: Option<Arc<dyn Fn(GrepArgs) -> ExecFuture<GrepResult> + Send + Sync>>,
    pub write: Option<Arc<dyn Fn(WriteArgs) -> ExecFuture<WriteResult> + Send + Sync>>,
    pub delete: Option<Arc<dyn Fn(DeleteArgs) -> ExecFuture<DeleteResult> + Send + Sync>>,
    pub shell: Option<Arc<dyn Fn(ShellArgs) -> ExecFuture<ShellResult> + Send + Sync>>,
    pub shell_stream: Option<
        Arc<
            dyn Fn(ShellArgs, Box<dyn CursorShellStreamCallbacks>) -> ExecFuture<ShellResult> + Send + Sync,
        >,
    >,
    pub diagnostics: Option<Arc<dyn Fn(DiagnosticsArgs) -> ExecFuture<DiagnosticsResult> + Send + Sync>>,
    pub mcp: Option<Arc<dyn Fn(CursorMcpCall) -> ExecFuture<McpResult> + Send + Sync>>,
    /// Answers "would this MCP call be permitted", without running it.
    ///
    /// `true` only when the host's policy resolves to a definite allow. A
    /// pending prompt is `false`: it can only be answered interactively at
    /// execution time. When no handler is registered the provider refuses,
    /// since it cannot decide.
    pub mcp_approval_preflight: Option<Arc<dyn Fn(CursorMcpCall) -> BoxFuture<'static, bool> + Send + Sync>>,
    /// Modern Cursor CLI Pi tool frames (`ExecServerMessage` 45-51). They are a
    /// distinct frame family from the legacy `readArgs`/`shellArgs`/... set:
    /// different args, different result oneofs, and no `tool_call_id`.
    pub pi_read: Option<Arc<dyn Fn(CursorPiCall<PiReadExecArgs>) -> ExecFuture<PiReadExecResult> + Send + Sync>>,
    pub pi_bash: Option<Arc<dyn Fn(CursorPiCall<PiBashExecArgs>) -> ExecFuture<PiBashExecResult> + Send + Sync>>,
    pub pi_edit: Option<Arc<dyn Fn(CursorPiCall<PiEditExecArgs>) -> ExecFuture<PiEditExecResult> + Send + Sync>>,
    pub pi_write: Option<Arc<dyn Fn(CursorPiCall<PiWriteExecArgs>) -> ExecFuture<PiWriteExecResult> + Send + Sync>>,
    pub pi_grep: Option<Arc<dyn Fn(CursorPiCall<PiGrepExecArgs>) -> ExecFuture<PiGrepExecResult> + Send + Sync>>,
    pub pi_find: Option<Arc<dyn Fn(CursorPiCall<PiFindExecArgs>) -> ExecFuture<PiFindExecResult> + Send + Sync>>,
    pub pi_ls: Option<Arc<dyn Fn(CursorPiCall<PiLsExecArgs>) -> ExecFuture<PiLsExecResult> + Send + Sync>>,
    pub on_tool_result: Option<CursorToolResultHandler>,
}

/// Stream options accepted by the `cursor-agent` API.
#[derive(Default)]
pub struct CursorAgentOptions {
    pub base: StreamOptions,
    /// Optional server-side system prompt override (RunRequest.customSystemPrompt).
    pub custom_system_prompt: Option<String>,
    /// Conversation id override; defaults to `sessionId`, else a random UUID per stream.
    pub conversation_id: Option<String>,
    /// Cursor exec/MCP tool handlers supplied by the host.
    pub exec_handlers: Option<CursorExecHandlers>,
    /// Receives every exec-channel tool result for transcript pairing.
    pub on_tool_result: Option<CursorToolResultHandler>,
    /// Provenance-bearing thinking selection rendered into
    /// `RequestedModel.parameters`; absent selections keep the representative
    /// variant request shape.
    pub thinking_selection: Option<ThinkingSelection>,
    /// Override stream health bounds for deterministic provider integration tests.
    pub stream_health_fail_threshold_ms: Option<u64>,
    /// Maximum pre-completion stall/transport retries (default 10).
    pub stream_stall_max_retries: Option<u32>,
    /// Fixed retry delay for tests; production uses exponential backoff plus jitter.
    pub stream_stall_retry_delay_ms: Option<u64>,
    /// Override the post-turn exec drain bound for deterministic tests.
    pub turn_end_drain_timeout_ms: Option<u64>,
}
