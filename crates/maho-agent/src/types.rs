//! Port of senpi packages/agent/src/types.ts.
//!
//! `AgentMessage` is `Message | CustomAgentMessages[keyof CustomAgentMessages]` in TS. The
//! declaration-merged custom union is empty by default, so it is modelled as the uninhabited
//! `CustomAgentMessage` variant: today it is isomorphic to `Message`, and a later todo that adds
//! custom messages extends the enum instead of every call site.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use maho_ai::api::cursor_agent::types::{CursorExecHandlers, CursorToolResultHandler};
use maho_ai::model::Model;
use maho_ai::types::{
    AssistantMessage, BoxFuture, ContentBlock, Context, Message, ModelThinkingLevel, SimpleStreamOptions,
    ThinkingLevel, ThinkingSelection, Tool, ToolCall, ToolResultMessage, Usage,
};
use maho_ai::utils::abort::AbortSignal;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Stream function used by the agent loop. `Models.streamSimple` satisfies this shape.
///
/// Contract:
/// - Must not throw or return a rejected promise for request/model/runtime failures.
/// - Must return an `AssistantMessageEventStream`.
/// - Failures must be encoded in the returned stream via protocol events and a final
///   `AssistantMessage` with stopReason "error" or "aborted" and errorMessage.
pub type StreamFn = Arc<
    dyn Fn(&Model, &Context, Option<AgentStreamOptions>) -> maho_ai::types::AssistantMessageEventStream + Send + Sync,
>;

/// senpi's `SimpleStreamOptions & CursorAgentOptions`: the loop hands the stream function one
/// options object that carries both the provider-neutral request options and the Cursor exec
/// channel handlers (TS structural typing; Rust needs the two in one struct).
#[derive(Clone, Default)]
pub struct AgentStreamOptions {
    pub simple: SimpleStreamOptions,
    /// Cursor exec-channel tool handlers (cursor-agent models only). Other providers ignore them.
    ///
    /// TS hands the handler table to the stream function by reference and the loop and the provider
    /// hold it at the same time; the table is not `Clone`, so the port shares it through an `Arc`.
    pub exec_handlers: Option<Arc<CursorExecHandlers>>,
    /// Receives every exec-channel tool result for transcript pairing.
    pub on_tool_result: Option<CursorToolResultHandler>,
}

/// `CursorExecHandlers | ((runSignal: AbortSignal) => CursorExecHandlers)`.
#[derive(Clone)]
pub enum CursorExecHandlersConfig {
    Handlers(Arc<CursorExecHandlers>),
    Factory(Arc<dyn Fn(Option<AbortSignal>) -> Arc<CursorExecHandlers> + Send + Sync>),
}

/// `ThinkingLevel` parsed from a runtime string (`this._state.reasoningBaseline as ThinkingLevel`).
///
/// `"off"` has no `ThinkingLevel` member, exactly as in TS where it is then dropped; an unknown
/// string is likewise dropped instead of flowing on as an untyped value.
pub(crate) fn parse_thinking_level(value: &str) -> Option<ThinkingLevel> {
    match value {
        "minimal" => Some(ThinkingLevel::Minimal),
        "low" => Some(ThinkingLevel::Low),
        "medium" => Some(ThinkingLevel::Medium),
        "high" => Some(ThinkingLevel::High),
        "xhigh" => Some(ThinkingLevel::Xhigh),
        "max" => Some(ThinkingLevel::Max),
        _ => None,
    }
}

/// `thinkingLevel === "off" ? undefined : thinkingLevel` for the model-level enum.
pub(crate) fn model_thinking_level_to_reasoning(level: ModelThinkingLevel) -> Option<ThinkingLevel> {
    match level {
        ModelThinkingLevel::Off => None,
        ModelThinkingLevel::Minimal => Some(ThinkingLevel::Minimal),
        ModelThinkingLevel::Low => Some(ThinkingLevel::Low),
        ModelThinkingLevel::Medium => Some(ThinkingLevel::Medium),
        ModelThinkingLevel::High => Some(ThinkingLevel::High),
        ModelThinkingLevel::Xhigh => Some(ThinkingLevel::Xhigh),
        ModelThinkingLevel::Max => Some(ThinkingLevel::Max),
    }
}

/// Configuration for how tool calls from a single assistant message are executed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ToolExecutionMode {
    /// Each tool call is prepared, executed, and finalized before the next one starts.
    Sequential,
    /// Tool calls are prepared sequentially, then scheduled in concurrent waves.
    Parallel,
}

/// Controls how many queued user messages are injected at a queue drain point.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum QueueMode {
    All,
    OneAtATime,
}

/// The two host queues a drain can belong to (`"steering" | "followUp"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PendingQueue {
    Steering,
    FollowUp,
}

/// A single tool call content block emitted by an assistant message.
pub type AgentToolCall = ToolCall;

/// Result returned from `beforeToolCall`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BeforeToolCallResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Hint that the agent should stop after the current tool batch when this call is blocked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminate: Option<bool>,
}

/// Partial override returned from `afterToolCall`. Merge semantics are field-by-field.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AfterToolCallResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<Vec<ContentBlock>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminate: Option<bool>,
}

/// Context passed to `beforeToolCall`.
#[derive(Debug, Clone)]
pub struct BeforeToolCallContext {
    pub assistant_message: AssistantMessage,
    pub tool_call: AgentToolCall,
    pub args: Value,
    pub context: AgentContext,
}

/// Context passed to `afterToolCall`.
#[derive(Debug, Clone)]
pub struct AfterToolCallContext {
    pub assistant_message: AssistantMessage,
    pub tool_call: AgentToolCall,
    pub args: Value,
    pub result: AgentToolResult,
    pub is_error: bool,
    pub context: AgentContext,
}

/// Context passed to `shouldStopAfterTurn`.
#[derive(Debug, Clone)]
pub struct ShouldStopAfterTurnContext {
    pub message: AssistantMessage,
    pub tool_results: Vec<ToolResultMessage>,
    pub context: AgentContext,
    pub new_messages: Vec<AgentMessage>,
}

/// `PrepareNextTurnContext` is the same shape as `ShouldStopAfterTurnContext`.
pub type PrepareNextTurnContext = ShouldStopAfterTurnContext;

/// Replacement runtime state used by the agent loop before starting another provider request.
#[derive(Debug, Clone, Default)]
pub struct AgentLoopTurnUpdate {
    pub context: Option<AgentContext>,
    pub model: Option<Model>,
    /// `undefined` leaves the level unchanged; `Some(Off)` clears it; any other level replaces it.
    pub thinking_level: Option<ModelThinkingLevel>,
    /// `undefined` leaves the selection unchanged; `Some(None)` clears it.
    pub thinking_selection: Option<Option<ThinkingSelection>>,
    pub abort_server_side_fallback: Option<bool>,
}

pub type ConvertToLlm = Arc<dyn Fn(Vec<AgentMessage>) -> BoxFuture<'static, Vec<Message>> + Send + Sync>;
pub type TransformContext =
    Arc<dyn Fn(Vec<AgentMessage>, Option<AbortSignal>) -> BoxFuture<'static, Vec<AgentMessage>> + Send + Sync>;
pub type GetApiKey = Arc<dyn Fn(String) -> BoxFuture<'static, Option<String>> + Send + Sync>;
pub type GetMessages = Arc<dyn Fn() -> BoxFuture<'static, Vec<AgentMessage>> + Send + Sync>;
pub type RestorePendingMessages =
    Arc<dyn Fn(PendingQueue, Vec<AgentMessage>) -> BoxFuture<'static, ()> + Send + Sync>;
pub type ShouldStopAfterTurn =
    Arc<dyn Fn(ShouldStopAfterTurnContext) -> BoxFuture<'static, bool> + Send + Sync>;
pub type PrepareNextTurn =
    Arc<dyn Fn(PrepareNextTurnContext) -> BoxFuture<'static, Option<AgentLoopTurnUpdate>> + Send + Sync>;
pub type ResolveUnknownToolCall =
    Arc<dyn Fn(String, AgentContext) -> BoxFuture<'static, Option<AgentTool>> + Send + Sync>;
pub type BeforeToolCall =
    Arc<dyn Fn(BeforeToolCallContext, Option<AbortSignal>) -> BoxFuture<'static, Option<BeforeToolCallResult>> + Send + Sync>;
pub type AfterToolCall =
    Arc<dyn Fn(AfterToolCallContext, Option<AbortSignal>) -> BoxFuture<'static, Option<AfterToolCallResult>> + Send + Sync>;

/// Configuration for the agent loop (`AgentLoopConfig extends SimpleStreamOptions`).
#[derive(Clone)]
pub struct AgentLoopConfig {
    pub model: Model,
    /// The request options the loop hands the stream function; the loop sets streamKind, apiKey
    /// and signal per request, exactly as the TS object spread does.
    pub options: SimpleStreamOptions,
    /// Cursor exec-channel tool handlers (cursor-agent models only).
    pub cursor_exec_handlers: Option<CursorExecHandlersConfig>,
    /// Maximum time in milliseconds to wait for the FIRST provider stream event.
    pub stream_start_timeout_ms: Option<u64>,
    /// Provider/SDK timeout override for only the first request in this loop invocation.
    pub initial_request_timeout_ms: Option<u64>,
    /// Stream-start timeout override for only the first request in this loop invocation.
    pub initial_request_stream_start_timeout_ms: Option<u64>,
    /// Converts AgentMessage[] to LLM-compatible Message[] before each LLM call.
    pub convert_to_llm: ConvertToLlm,
    /// Optional transform applied to the context before `convertToLlm`.
    pub transform_context: Option<TransformContext>,
    /// Resolves an API key dynamically for each LLM call.
    pub get_api_key: Option<GetApiKey>,
    /// Called after each turn fully completes and `turn_end` has been emitted.
    pub should_stop_after_turn: Option<ShouldStopAfterTurn>,
    /// Called after each completed assistant turn, before the loop decides whether another request starts.
    pub prepare_next_turn: Option<PrepareNextTurn>,
    /// Returns steering messages to inject into the conversation mid-run.
    pub get_steering_messages: Option<GetMessages>,
    /// Returns follow-up messages to process after the agent would otherwise stop.
    pub get_follow_up_messages: Option<GetMessages>,
    /// Restores messages previously returned by a queue callback when next-turn preparation cannot continue.
    pub restore_pending_messages: Option<RestorePendingMessages>,
    /// Tool execution mode. Default: "parallel".
    pub tool_execution: Option<ToolExecutionMode>,
    /// Optional migration guidance for tool names intentionally removed by an extension.
    pub removed_tool_hints: Option<BTreeMap<String, String>>,
    /// Called when a model names a tool absent from the current context snapshot.
    pub resolve_unknown_tool_call: Option<ResolveUnknownToolCall>,
    /// Called before a tool is executed, after arguments have been validated.
    pub before_tool_call: Option<BeforeToolCall>,
    /// Called after a tool finishes executing, before `tool_execution_end` is emitted.
    pub after_tool_call: Option<AfterToolCall>,
}

impl AgentLoopConfig {
    /// The TS default `convertToLlm` is supplied by every host; the identity conversion
    /// (every AgentMessage is already an LLM Message) is the only one this crate can default to.
    pub fn new(model: Model, convert_to_llm: ConvertToLlm) -> Self {
        Self {
            model,
            options: SimpleStreamOptions::default(),
            cursor_exec_handlers: None,
            stream_start_timeout_ms: None,
            initial_request_timeout_ms: None,
            initial_request_stream_start_timeout_ms: None,
            convert_to_llm,
            transform_context: None,
            get_api_key: None,
            should_stop_after_turn: None,
            prepare_next_turn: None,
            get_steering_messages: None,
            get_follow_up_messages: None,
            restore_pending_messages: None,
            tool_execution: None,
            removed_tool_hints: None,
            resolve_unknown_tool_call: None,
            before_tool_call: None,
            after_tool_call: None,
        }
    }

    pub fn tool_execution_mode(&self) -> ToolExecutionMode {
        self.tool_execution.unwrap_or(ToolExecutionMode::Parallel)
    }

    /// `config.timeoutMs` in TS (the idle timeout carried on the request options).
    pub fn timeout_ms(&self) -> Option<u64> {
        self.options.stream.request.timeout_ms
    }
}

/// Identity `convertToLlm`: every `AgentMessage` in this crate is already an LLM `Message`.
pub fn identity_convert_to_llm() -> ConvertToLlm {
    Arc::new(|messages: Vec<AgentMessage>| {
        Box::pin(async move { messages.into_iter().map(AgentMessage::into_llm).collect() })
    })
}

/// Extensible interface for custom app messages (empty by default, as in senpi).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum CustomAgentMessage {}

/// AgentMessage: union of LLM messages + custom messages.
///
/// The size gap between the arms comes from the declaration-merged custom union being empty here;
/// boxing the LLM arm would put an indirection the TS union does not have on every message.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AgentMessage {
    Llm(Message),
    Custom(CustomAgentMessage),
}

impl AgentMessage {
    pub fn role(&self) -> &'static str {
        match self {
            AgentMessage::Llm(message) => message.role(),
            AgentMessage::Custom(never) => match *never {},
        }
    }

    pub fn as_llm(&self) -> &Message {
        match self {
            AgentMessage::Llm(message) => message,
            AgentMessage::Custom(never) => match *never {},
        }
    }

    pub fn into_llm(self) -> Message {
        match self {
            AgentMessage::Llm(message) => message,
            AgentMessage::Custom(never) => match never {},
        }
    }

    pub fn as_assistant(&self) -> Option<&AssistantMessage> {
        match self.as_llm() {
            Message::Assistant(message) => Some(message),
            _ => None,
        }
    }

    pub fn as_tool_result(&self) -> Option<&ToolResultMessage> {
        match self.as_llm() {
            Message::ToolResult(message) => Some(message),
            _ => None,
        }
    }
}

impl From<Message> for AgentMessage {
    fn from(message: Message) -> Self {
        AgentMessage::Llm(message)
    }
}

/// Public agent state. `tools` and `messages` copy the assigned array, as the TS accessors do.
#[derive(Debug, Clone)]
pub struct AgentState {
    pub system_prompt: String,
    pub model: Model,
    pub thinking_level: ModelThinkingLevel,
    pub thinking_selection: Option<ThinkingSelection>,
    pub reasoning_baseline: Option<String>,
    tools: Vec<AgentTool>,
    messages: Vec<AgentMessage>,
    pub is_streaming: bool,
    pub streaming_message: Option<AgentMessage>,
    pub pending_tool_calls: BTreeSet<String>,
    pub error_message: Option<String>,
}

impl AgentState {
    pub fn new(system_prompt: String, model: Model) -> Self {
        Self {
            system_prompt,
            model,
            thinking_level: ModelThinkingLevel::Off,
            thinking_selection: None,
            reasoning_baseline: None,
            tools: Vec::new(),
            messages: Vec::new(),
            is_streaming: false,
            streaming_message: None,
            pending_tool_calls: BTreeSet::new(),
            error_message: None,
        }
    }

    /// Assigning a new array copies the top-level array.
    pub fn set_tools(&mut self, tools: Vec<AgentTool>) {
        self.tools = tools;
    }

    pub fn tools(&self) -> &[AgentTool] {
        &self.tools
    }

    pub fn set_messages(&mut self, messages: Vec<AgentMessage>) {
        self.messages = messages;
    }

    pub fn messages(&self) -> &[AgentMessage] {
        &self.messages
    }
}

/// Final or partial result produced by a tool.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentToolResult {
    /// Text or image content returned to the model.
    pub content: Vec<ContentBlock>,
    /// Arbitrary structured details for logs or UI rendering.
    pub details: Value,
    /// Usage from the final tool execution itself, if available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    /// Names of tools introduced by this result and available from this transcript point onward.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub added_tool_names: Option<Vec<String>>,
    /// Hint that the agent should stop after the current tool batch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminate: Option<bool>,
    /// Report a failure without throwing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
}

impl AgentToolResult {
    pub fn text(message: impl Into<String>) -> Self {
        Self {
            content: vec![ContentBlock::text(message)],
            details: Value::Object(Default::default()),
            usage: None,
            added_tool_names: None,
            terminate: None,
            is_error: None,
        }
    }
}

/// Callback used by tools to stream partial execution updates.
pub type AgentToolUpdateCallback = Arc<dyn Fn(AgentToolResult) + Send + Sync>;

/// Compatibility shim for raw tool-call arguments before schema validation.
pub type ToolArgumentShim = Arc<dyn Fn(Value) -> Value + Send + Sync>;

/// Tool execution function: `(toolCallId, params, signal?, onUpdate?)`.
pub type AgentToolExecute = Arc<
    dyn Fn(String, Value, Option<AbortSignal>, Option<AgentToolUpdateCallback>) -> BoxFuture<'static, AgentToolResult>
        + Send
        + Sync,
>;

/// Recovery policy for an effect whose durable intent exists but whose outcome is unknown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReplayPolicy {
    Never,
    Safe,
}

/// Tool definition used by the agent runtime.
#[derive(Clone)]
pub struct AgentTool {
    /// Human-readable label for UI display.
    pub label: String,
    /// Optional compatibility shim for raw tool-call arguments before schema validation.
    pub prepare_arguments: Option<ToolArgumentShim>,
    /// Execute the tool call. Throw on failure instead of encoding errors in `content`.
    pub execute: AgentToolExecute,
    pub replay: Option<ReplayPolicy>,
    /// Per-tool execution mode override.
    pub execution_mode: Option<ToolExecutionMode>,
    /// The provider-facing tool definition this agent tool wraps.
    pub tool: Tool,
}

impl AgentTool {
    pub fn name(&self) -> &str {
        &self.tool.name
    }
}

impl std::fmt::Debug for AgentTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentTool")
            .field("name", &self.tool.name)
            .field("label", &self.label)
            .field("execution_mode", &self.execution_mode)
            .finish_non_exhaustive()
    }
}

/// Context snapshot passed into the low-level agent loop.
#[derive(Debug, Clone, Default)]
pub struct AgentContext {
    /// System prompt included with the request.
    pub system_prompt: String,
    /// Transcript visible to the model.
    pub messages: Vec<AgentMessage>,
    /// Tools available for this run.
    pub tools: Option<Vec<AgentTool>>,
}

/// Events emitted by the Agent for UI updates.
///
/// One flat discriminated union, as in TS; boxing the streaming arm's message would change the
/// public event shape every consumer matches on.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum AgentEvent {
    // Agent lifecycle
    AgentStart,
    AgentEnd {
        messages: Vec<AgentMessage>,
    },
    // Turn lifecycle - a turn is one assistant response + any tool calls/results
    TurnStart,
    TurnEnd {
        message: AgentMessage,
        tool_results: Vec<ToolResultMessage>,
    },
    // Message lifecycle - emitted for user, assistant, and toolResult messages
    MessageStart {
        message: AgentMessage,
    },
    /// Only emitted for assistant messages during streaming.
    MessageUpdate {
        message: AgentMessage,
        assistant_message_event: maho_ai::types::AssistantMessageEvent,
    },
    MessageEnd {
        message: AgentMessage,
    },
    // Tool execution lifecycle
    ToolExecutionStart {
        tool_call_id: String,
        tool_name: String,
        args: Value,
    },
    ToolExecutionUpdate {
        tool_call_id: String,
        tool_name: String,
        args: Value,
        partial_result: Value,
    },
    ToolExecutionEnd {
        tool_call_id: String,
        tool_name: String,
        result: Value,
        is_error: bool,
    },
}
