//! Port of senpi packages/agent/src/agent.ts.
//!
//! Stateful wrapper around the low-level agent loop. The TS class is shared by reference and
//! mutates its own fields from concurrent callbacks, so this port keeps the same shape with an
//! `Arc<AgentInner>` and interior mutability: `Agent` is a cheap clone of that handle.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use maho_ai::model::Model;
use maho_ai::types::{
    ContentBlock, ImageContent, Message, ModelThinkingLevel, OnPayload, OnResponse,
    ProviderRequestOptions, SimpleStreamOptions, ThinkingBudgets, ThinkingSelection, Transport,
    Usage, UsageCost, UserContent, UserMessage,
};
use maho_ai::utils::abort::{AbortController, AbortSignal};

use crate::agent_loop::{
    AgentEventSink, build_provider_context, run_agent_loop, run_agent_loop_continue,
};
use crate::assistant_terminal_state::now_ms;
use crate::stream_fn::get_default_stream_fn;
use crate::types::{
    AfterToolCall, AfterToolCallContext, AfterToolCallResult, AgentContext, AgentEvent,
    AgentLoopConfig, AgentLoopTurnUpdate, AgentMessage, AgentState, AgentTool, BeforeToolCall,
    BeforeToolCallContext, BeforeToolCallResult, ConvertToLlm, CursorExecHandlersConfig, GetApiKey,
    PendingQueue, PrepareNextTurnContext, QueueMode, ResolveUnknownToolCall,
    ShouldStopAfterTurnContext, StreamFn, ToolExecutionMode, TransformContext,
    model_thinking_level_to_reasoning, parse_thinking_level,
};

/// \`shouldStopAfterTurn\` as the host passes it: it also receives the active run signal.
pub type ShouldStopAfterTurnWithSignal = Arc<
    dyn Fn(
            ShouldStopAfterTurnContext,
            Option<AbortSignal>,
        ) -> maho_ai::types::BoxFuture<'static, bool>
        + Send
        + Sync,
>;

/// \`prepareNextTurn\` as the host passes it: it receives only the active run signal.
pub type PrepareNextTurnWithoutContext = Arc<
    dyn Fn(Option<AbortSignal>) -> maho_ai::types::BoxFuture<'static, Option<AgentLoopTurnUpdate>>
        + Send
        + Sync,
>;

/// \`prepareNextTurnWithContext\` as the host passes it: it also receives the active run signal.
pub type PrepareNextTurnWithContext = Arc<
    dyn Fn(
            PrepareNextTurnContext,
            Option<AbortSignal>,
        ) -> maho_ai::types::BoxFuture<'static, Option<AgentLoopTurnUpdate>>
        + Send
        + Sync,
>;

pub type AgentListener =
    Arc<dyn Fn(AgentEvent, AbortSignal) -> maho_ai::types::BoxFuture<'static, ()> + Send + Sync>;

pub type QueuedDrain = Arc<dyn Fn() -> Vec<AgentMessage> + Send + Sync>;

pub struct QueuedGiveBack {
    pub restorable: Arc<dyn Fn() -> bool + Send + Sync>,
    pub restore: Arc<dyn Fn() -> bool + Send + Sync>,
}

pub trait QueuedInputPort: Send + Sync {
    fn claim(
        &self,
        queue: PendingQueue,
        drain: QueuedDrain,
    ) -> maho_ai::types::BoxFuture<'static, Vec<AgentMessage>>;
    fn release(
        &self,
        queue: PendingQueue,
        messages: Vec<AgentMessage>,
        give_back: QueuedGiveBack,
    ) -> maho_ai::types::BoxFuture<'static, ()>;
    fn commit_consumption(
        &self,
        queue: PendingQueue,
        messages: Vec<AgentMessage>,
    ) -> maho_ai::types::BoxFuture<'static, Result<(), String>>;
}

fn default_convert_to_llm(messages: Vec<AgentMessage>) -> Vec<Message> {
    messages
        .into_iter()
        .filter(|message| {
            matches!(
                message.role(),
                "system" | "user" | "assistant" | "toolResult" | "configurationUpdate"
            )
        })
        .map(AgentMessage::into_llm)
        .collect()
}

fn empty_usage() -> Usage {
    Usage {
        context_usage: None,
        cost_unknown: None,
        input: 0,
        output: 0,
        cache_read: 0,
        cache_write: 0,
        cache_write_1h: None,
        reasoning: None,
        total_tokens: 0,
        cost: UsageCost::default(),
    }
}

pub(crate) fn default_model() -> Model {
    Model {
        id: "unknown".to_owned(),
        name: "unknown".to_owned(),
        api: "unknown".to_owned(),
        provider: "unknown".to_owned(),
        base_url: String::new(),
        reasoning: false,
        default_thinking_level: None,
        thinking_level_map: None,
        input: Vec::new(),
        cost: maho_ai::types::ModelCost { unknown: None,
            effective_rates: None,
            input: 0.0,
            output: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
            tiers: None,
        },
        context_window: 0,
        max_tokens: 0,
        sampling_params: None,
        headers: None,
        cache_retention: None,
        prompt_cache: None,
        supports_assistant_prefill: None,
        upstream_model_id: None,
        deprecated: None,
        service_tier: None,
        recover_text_tool_calls: None,
        compat: None,
        input_limits: None,
    }
}

/// \`Partial<Omit<AgentState, "pendingToolCalls" | "isStreaming" | "streamingMessage" | "errorMessage">>\`.
#[derive(Clone, Default)]
pub struct PartialAgentState {
    pub system_prompt: Option<String>,
    pub model: Option<Model>,
    pub thinking_level: Option<ModelThinkingLevel>,
    pub thinking_selection: Option<ThinkingSelection>,
    pub reasoning_baseline: Option<String>,
    pub tools: Option<Vec<AgentTool>>,
    pub declared_tools: Option<Vec<AgentTool>>,
    pub messages: Option<Vec<AgentMessage>>,
}

/// Options for constructing an [`Agent`].
#[derive(Default)]
pub struct AgentOptions {
    pub initial_state: Option<PartialAgentState>,
    pub convert_to_llm: Option<ConvertToLlm>,
    pub transform_context: Option<TransformContext>,
    pub stream_fn: Option<StreamFn>,
    pub get_api_key: Option<GetApiKey>,
    pub on_payload: Option<OnPayload>,
    pub on_response: Option<OnResponse>,
    pub before_tool_call: Option<BeforeToolCall>,
    pub after_tool_call: Option<AfterToolCall>,
    pub should_stop_after_turn: Option<ShouldStopAfterTurnWithSignal>,
    pub finish_turn: Option<crate::types::FinishTurn>,
    pub prepare_request: Option<crate::types::PrepareRequest>,
    pub queued_input_port: Option<Arc<dyn QueuedInputPort>>,
    pub prepare_next_turn: Option<PrepareNextTurnWithoutContext>,
    pub prepare_next_turn_with_context: Option<PrepareNextTurnWithContext>,
    pub steering_mode: Option<QueueMode>,
    pub follow_up_mode: Option<QueueMode>,
    pub session_id: Option<String>,
    pub thinking_budgets: Option<ThinkingBudgets>,
    pub transport: Option<Transport>,
    pub timeout_ms: Option<u64>,
    pub stream_start_timeout_ms: Option<u64>,
    pub max_retry_delay_ms: Option<u64>,
    pub tool_execution: Option<ToolExecutionMode>,
    pub removed_tool_hints: Option<BTreeMap<String, String>>,
    pub resolve_unknown_tool_call: Option<ResolveUnknownToolCall>,
    pub abort_server_side_fallback: Option<bool>,
    /// Cursor exec-channel tool handlers; see [`AgentLoopConfig::cursor_exec_handlers`].
    pub cursor_exec_handlers: Option<CursorExecHandlersConfig>,
}

#[derive(Clone, Default)]
pub struct AgentContinuationOptions {
    /// Keep queued steering and follow-up input out of the continuation's first provider request only.
    pub defer_queued_messages: Option<bool>,
    /// Override the provider stream idle timeout for the continuation's first provider request only.
    pub timeout_ms: Option<u64>,
    /// Override the provider stream-start timeout for the continuation's first provider request only.
    pub stream_start_timeout_ms: Option<u64>,
}

struct PendingMessageQueue {
    messages: Vec<AgentMessage>,
    clear_generation: u64,
    mode: QueueMode,
}

impl PendingMessageQueue {
    fn new(mode: QueueMode) -> Self {
        Self {
            messages: Vec::new(),
            clear_generation: 0,
            mode,
        }
    }

    fn enqueue(&mut self, message: AgentMessage) {
        self.messages.push(message);
    }

    fn has_items(&self) -> bool {
        !self.messages.is_empty()
    }

    fn clear_generation(&self) -> u64 {
        self.clear_generation
    }

    fn drain(&mut self) -> Vec<AgentMessage> {
        if self.mode == QueueMode::All {
            return std::mem::take(&mut self.messages);
        }
        if self.messages.is_empty() {
            return Vec::new();
        }
        let end = if is_background_notice(&self.messages[0]) {
            self.messages
                .iter()
                .take_while(|message| is_background_notice(message))
                .count()
        } else {
            1
        };
        self.messages.drain(..end).collect()
    }

    fn peek(&self) -> Vec<AgentMessage> {
        let mut snapshot = Self {
            messages: self.messages.clone(),
            clear_generation: self.clear_generation,
            mode: self.mode,
        };
        snapshot.drain()
    }

    fn prepend(&mut self, messages: Vec<AgentMessage>) {
        let mut combined = messages;
        combined.append(&mut self.messages);
        self.messages = combined;
    }

    fn remove(&mut self, message: &AgentMessage) -> bool {
        if let Some(index) = self.messages.iter().position(|m| m == message) {
            self.messages.remove(index);
            true
        } else {
            false
        }
    }

    fn clear(&mut self) {
        self.messages.clear();
        self.clear_generation += 1;
    }
}

fn is_background_notice(message: &AgentMessage) -> bool {
    !matches!(
        message.role(),
        "system" | "user" | "assistant" | "toolResult"
    )
}

struct ActiveRun {
    completion: tokio::sync::watch::Sender<bool>,
    abort_controller: AbortController,
    suppress_queued_message_drain: bool,
}

struct Listeners {
    next_id: u64,
    entries: Vec<(u64, AgentListener)>,
}

/// Handle returned by [`Agent::subscribe`]; pass it back to [`Agent::unsubscribe`].
#[derive(Clone)]
pub struct AgentSubscription {
    id: u64,
}

impl AgentSubscription {
    pub fn id(&self) -> u64 {
        self.id
    }
}

struct AgentInner {
    state: Mutex<AgentState>,
    listeners: Mutex<Listeners>,
    steering_queue: Mutex<PendingMessageQueue>,
    follow_up_queue: Mutex<PendingMessageQueue>,
    active_run: Mutex<Option<ActiveRun>>,
    settings: Mutex<AgentSettings>,
}

struct AgentSettings {
    convert_to_llm: ConvertToLlm,
    transform_context: Option<TransformContext>,
    stream_function: StreamFn,
    get_api_key: Option<GetApiKey>,
    on_payload: Option<OnPayload>,
    on_response: Option<OnResponse>,
    before_tool_call: Option<BeforeToolCall>,
    after_tool_call: Option<AfterToolCall>,
    should_stop_after_turn: Option<ShouldStopAfterTurnWithSignal>,
    finish_turn: Option<crate::types::FinishTurn>,
    prepare_request: Option<crate::types::PrepareRequest>,
    queued_input_port: Option<Arc<dyn QueuedInputPort>>,
    prepare_next_turn: Option<PrepareNextTurnWithoutContext>,
    prepare_next_turn_with_context: Option<PrepareNextTurnWithContext>,
    session_id: Option<String>,
    thinking_budgets: Option<ThinkingBudgets>,
    transport: Option<Transport>,
    timeout_ms: Option<u64>,
    stream_start_timeout_ms: Option<u64>,
    max_retry_delay_ms: Option<u64>,
    tool_execution: ToolExecutionMode,
    removed_tool_hints: BTreeMap<String, String>,
    resolve_unknown_tool_call: Option<ResolveUnknownToolCall>,
    abort_server_side_fallback: Option<bool>,
    cursor_exec_handlers: Option<CursorExecHandlersConfig>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Stateful wrapper around the low-level agent loop.
///
/// `Agent` owns the current transcript, emits lifecycle events, executes tools,
/// and exposes queueing APIs for steering and follow-up messages.
#[derive(Clone)]
pub struct Agent {
    inner: Arc<AgentInner>,
}

impl Agent {
    pub fn new(options: AgentOptions) -> Self {
        let initial_state = options.initial_state.clone().unwrap_or_default();
        let mut state = AgentState::new(
            initial_state.system_prompt.clone().unwrap_or_default(),
            initial_state.model.clone().unwrap_or_else(default_model),
        );
        state.thinking_level = initial_state
            .thinking_level
            .unwrap_or(ModelThinkingLevel::Off);
        state.thinking_selection = initial_state.thinking_selection.clone();
        state.reasoning_baseline = initial_state.reasoning_baseline.clone();
        state.declared_tools = initial_state.declared_tools.clone();
        state.set_tools(initial_state.tools.clone().unwrap_or_default());
        state.set_messages(initial_state.messages.clone().unwrap_or_default());

        let settings = AgentSettings {
            convert_to_llm: options.convert_to_llm.clone().unwrap_or_else(|| {
                Arc::new(|messages| Box::pin(async move { default_convert_to_llm(messages) }))
            }),
            transform_context: options.transform_context.clone(),
            stream_function: options
                .stream_fn
                .clone()
                .unwrap_or_else(get_default_stream_fn),
            get_api_key: options.get_api_key.clone(),
            on_payload: options.on_payload.clone(),
            on_response: options.on_response.clone(),
            before_tool_call: options.before_tool_call.clone(),
            after_tool_call: options.after_tool_call.clone(),
            should_stop_after_turn: options.should_stop_after_turn.clone(),
            finish_turn: options.finish_turn.clone(),
            prepare_request: options.prepare_request.clone(),
            queued_input_port: options.queued_input_port.clone(),
            prepare_next_turn: options.prepare_next_turn.clone(),
            prepare_next_turn_with_context: options.prepare_next_turn_with_context.clone(),
            session_id: options.session_id.clone(),
            thinking_budgets: options.thinking_budgets.clone(),
            transport: options.transport,
            timeout_ms: options.timeout_ms,
            stream_start_timeout_ms: options.stream_start_timeout_ms,
            max_retry_delay_ms: options.max_retry_delay_ms,
            tool_execution: options
                .tool_execution
                .unwrap_or(ToolExecutionMode::Parallel),
            removed_tool_hints: options.removed_tool_hints.clone().unwrap_or_default(),
            resolve_unknown_tool_call: options.resolve_unknown_tool_call.clone(),
            abort_server_side_fallback: options.abort_server_side_fallback,
            cursor_exec_handlers: options.cursor_exec_handlers.clone(),
        };

        Self {
            inner: Arc::new(AgentInner {
                state: Mutex::new(state),
                listeners: Mutex::new(Listeners {
                    next_id: 0,
                    entries: Vec::new(),
                }),
                steering_queue: Mutex::new(PendingMessageQueue::new(
                    options.steering_mode.unwrap_or(QueueMode::OneAtATime),
                )),
                follow_up_queue: Mutex::new(PendingMessageQueue::new(
                    options.follow_up_mode.unwrap_or(QueueMode::OneAtATime),
                )),
                active_run: Mutex::new(None),
                settings: Mutex::new(settings),
            }),
        }
    }

    /// Subscribe to agent lifecycle events.
    ///
    /// Listener promises are awaited in subscription order and are included in
    /// the current run's settlement. Listeners also receive the active abort
    /// signal for the current run.
    ///
    /// `agent_end` is the final emitted event for a run, but the agent does not
    /// become idle until all awaited listeners for that event have settled.
    pub fn subscribe(&self, listener: AgentListener) -> AgentSubscription {
        let mut listeners = lock(&self.inner.listeners);
        let id = listeners.next_id;
        listeners.next_id += 1;
        listeners.entries.push((id, listener));
        AgentSubscription { id }
    }

    pub fn unsubscribe(&self, subscription: &AgentSubscription) {
        lock(&self.inner.listeners)
            .entries
            .retain(|(id, _)| *id != subscription.id);
    }

    /// Current agent state (a snapshot; the TS getter returns the live object).
    pub fn state(&self) -> AgentState {
        lock(&self.inner.state).clone()
    }

    /// `state.messages.length` without cloning the transcript.
    pub fn message_count(&self) -> usize {
        lock(&self.inner.state).messages().len()
    }

    /// `state.messages.length > 0` without cloning the transcript.
    pub fn has_messages(&self) -> bool {
        !lock(&self.inner.state).messages().is_empty()
    }

    /// `state.isStreaming || state.pendingToolCalls.size > 0`, read under one lock.
    pub fn is_streaming_or_has_pending_tool_calls(&self) -> bool {
        let state = lock(&self.inner.state);
        state.is_streaming || !state.pending_tool_calls.is_empty()
    }

    /// `state.tools.map(t => t.name)` without cloning the transcript or tool schemas.
    pub fn tool_names(&self) -> Vec<String> {
        lock(&self.inner.state)
            .tools()
            .iter()
            .map(|tool| tool.name().to_owned())
            .collect()
    }

    /// Active tool names plus the named active tool, both from one consistent snapshot.
    pub fn tool_names_and_find(&self, name: &str) -> (Vec<String>, Option<AgentTool>) {
        let state = lock(&self.inner.state);
        let names = state
            .tools()
            .iter()
            .map(|tool| tool.name().to_owned())
            .collect();
        let tool = state
            .tools()
            .iter()
            .find(|candidate| candidate.name() == name)
            .cloned();
        (names, tool)
    }

    /// `state.model` alone, without cloning the transcript and tool list.
    pub fn model(&self) -> Model {
        lock(&self.inner.state).model.clone()
    }

    /// `state.thinkingLevel` alone.
    pub fn thinking_level(&self) -> ModelThinkingLevel {
        lock(&self.inner.state).thinking_level
    }

    /// `state.thinkingSelection` alone.
    pub fn thinking_selection(&self) -> Option<ThinkingSelection> {
        lock(&self.inner.state).thinking_selection.clone()
    }

    /// `state.isStreaming` alone.
    pub fn is_streaming(&self) -> bool {
        lock(&self.inner.state).is_streaming
    }

    /// `state.systemPrompt` alone.
    pub fn system_prompt(&self) -> String {
        lock(&self.inner.state).system_prompt.clone()
    }

    /// Assign `state.tools`, copying the provided top-level array.
    pub fn set_tools(&self, tools: Vec<AgentTool>) {
        lock(&self.inner.state).set_tools(tools);
    }

    pub fn set_declared_tools(&self, tools: Option<Vec<AgentTool>>) {
        lock(&self.inner.state).declared_tools = tools;
    }

    /// Assign `state.messages`, copying the provided top-level array.
    pub fn set_messages(&self, messages: Vec<AgentMessage>) {
        lock(&self.inner.state).set_messages(messages);
    }

    pub fn set_system_prompt(&self, system_prompt: impl Into<String>) {
        lock(&self.inner.state).system_prompt = system_prompt.into();
    }

    pub fn set_model(&self, model: Model) {
        lock(&self.inner.state).model = model;
    }

    pub fn set_thinking_level(&self, level: ModelThinkingLevel) {
        lock(&self.inner.state).thinking_level = level;
    }

    /// Assign `state.thinkingSelection`.
    pub fn set_thinking_selection(&self, selection: Option<ThinkingSelection>) {
        lock(&self.inner.state).thinking_selection = selection;
    }

    /// `state.reasoningBaseline` alone.
    pub fn reasoning_baseline(&self) -> Option<String> {
        lock(&self.inner.state).reasoning_baseline.clone()
    }

    /// Assign `state.reasoningBaseline`.
    pub fn set_reasoning_baseline(&self, baseline: Option<String>) {
        lock(&self.inner.state).reasoning_baseline = baseline;
    }

    /// `state.reasoningBaseline ??= level`: keep the first pre-update reasoning level.
    pub fn init_reasoning_baseline(&self, level: &str) {
        lock(&self.inner.state)
            .reasoning_baseline
            .get_or_insert_with(|| level.to_owned());
    }

    pub fn steering_mode(&self) -> QueueMode {
        lock(&self.inner.steering_queue).mode
    }

    /// Controls how queued steering messages are drained.
    pub fn set_steering_mode(&self, mode: QueueMode) {
        lock(&self.inner.steering_queue).mode = mode;
    }

    pub fn follow_up_mode(&self) -> QueueMode {
        lock(&self.inner.follow_up_queue).mode
    }

    /// Controls how queued follow-up messages are drained.
    pub fn set_follow_up_mode(&self, mode: QueueMode) {
        lock(&self.inner.follow_up_queue).mode = mode;
    }

    /// Queue a message to be injected after the current assistant turn finishes.
    pub fn steer(&self, message: AgentMessage) {
        lock(&self.inner.steering_queue).enqueue(message);
    }

    /// Queue a message to run only after the agent would otherwise stop.
    pub fn follow_up(&self, message: AgentMessage) {
        lock(&self.inner.follow_up_queue).enqueue(message);
    }

    /// Remove all queued steering messages.
    pub fn clear_steering_queue(&self) {
        lock(&self.inner.steering_queue).clear();
    }

    /// Remove all queued follow-up messages.
    pub fn clear_follow_up_queue(&self) {
        lock(&self.inner.follow_up_queue).clear();
    }

    /// Remove all queued steering and follow-up messages.
    pub fn clear_all_queues(&self) {
        self.clear_steering_queue();
        self.clear_follow_up_queue();
    }

    /// Returns true when either queue still contains pending messages.
    pub fn has_queued_messages(&self) -> bool {
        lock(&self.inner.steering_queue).has_items()
            || lock(&self.inner.follow_up_queue).has_items()
    }

    /// Preview selected messages without consuming them; `all` includes both queues.
    pub fn peek_queued_messages(&self, all: bool) -> Vec<AgentMessage> {
        let mut steering = lock(&self.inner.steering_queue).peek();
        if all {
            steering.extend(lock(&self.inner.follow_up_queue).peek());
        } else if steering.is_empty() {
            return lock(&self.inner.follow_up_queue).peek();
        }
        steering
    }

    /// Returns true when `message` is still waiting in either pending queue.
    pub fn contains_queued_message(&self, message: &AgentMessage) -> bool {
        lock(&self.inner.steering_queue)
            .messages
            .iter()
            .any(|candidate| candidate == message)
            || lock(&self.inner.follow_up_queue)
                .messages
                .iter()
                .any(|candidate| candidate == message)
    }

    /// Remove this exact message from whichever queue holds it; false when it is no longer queued.
    pub fn remove_queued_message(&self, message: &AgentMessage) -> bool {
        lock(&self.inner.steering_queue).remove(message)
            || lock(&self.inner.follow_up_queue).remove(message)
    }

    /// Active abort signal for the current run, if any.
    pub fn signal(&self) -> Option<AbortSignal> {
        lock(&self.inner.active_run)
            .as_ref()
            .map(|run| run.abort_controller.signal())
    }

    /// Abort the current run, if one is active.
    pub fn abort(&self, reason: Option<maho_ai::utils::abort::AbortReason>) {
        if let Some(run) = lock(&self.inner.active_run).as_ref() {
            run.abort_controller.abort(reason);
        }
    }

    /// Keep queued steering and follow-up messages for an external owner after
    /// this run reaches agent_end, without changing the active abort signal.
    /// This is ownership suppression for one active run; terminal error/abort
    /// parking is a separate stop-reason policy enforced by the run lifecycle.
    pub fn suppress_queued_message_drain(&self) {
        if let Some(run) = lock(&self.inner.active_run).as_mut() {
            run.suppress_queued_message_drain = true;
        }
    }

    /// Resolve when the current run and all awaited event listeners have finished.
    ///
    /// This resolves after `agent_end` listeners settle.
    pub async fn wait_for_idle(&self) {
        let receiver = lock(&self.inner.active_run)
            .as_ref()
            .map(|run| run.completion.subscribe());
        if let Some(mut receiver) = receiver {
            let _ = receiver.wait_for(|done| *done).await;
        }
    }

    /// Clear transcript state, runtime state, and queued messages.
    pub fn reset(&self) {
        if lock(&self.inner.active_run).is_some() {
            panic!("Agent is already processing. Wait for completion before resetting.");
        }

        {
            let mut state = lock(&self.inner.state);
            state.set_messages(Vec::new());
            state.is_streaming = false;
            state.streaming_message = None;
            state.pending_tool_calls.clear();
            state.error_message = None;
            state.provider_diagnostic = None;
        }
        self.clear_follow_up_queue();
        self.clear_steering_queue();
    }

    /// Start a new prompt from text, a single message, or a batch of messages.
    pub async fn prompt(&self, input: AgentPromptInput) {
        if lock(&self.inner.active_run).is_some() {
            panic!(
                "Agent is already processing a prompt. Use steer() or followUp() to queue messages, or wait for completion."
            );
        }
        let messages = self.normalize_prompt_input(input);
        self.run_prompt_messages(messages, PromptRunOptions::default())
            .await;
    }

    /// Continue by delivering queued input first when a compaction leaves custom context at the tail.
    /// Queue-first recovery takes precedence over `deferQueuedMessages`: the selected queued message is
    /// the continuation input, while timeout overrides still apply to its first provider request.
    pub async fn continue_with_queued_messages(&self, options: AgentContinuationOptions) {
        if lock(&self.inner.active_run).is_some() {
            panic!("Agent is already processing. Wait for completion before continuing.");
        }

        let last_role = lock(&self.inner.state)
            .messages()
            .last()
            .map(|message| message.role().to_owned());
        if last_role.as_deref() == Some("assistant") {
            self.continue_run(options).await;
            return;
        }

        if lock(&self.inner.steering_queue).has_items() {
            self.run_queued_messages(
                PendingQueue::Steering,
                PromptRunOptions {
                    skip_initial_steering_poll: true,
                    initial_request_timeout_ms: options.timeout_ms,
                    initial_request_stream_start_timeout_ms: options.stream_start_timeout_ms,
                },
            )
            .await;
            return;
        }

        if lock(&self.inner.follow_up_queue).has_items() {
            self.run_queued_messages(
                PendingQueue::FollowUp,
                PromptRunOptions {
                    skip_initial_steering_poll: false,
                    initial_request_timeout_ms: options.timeout_ms,
                    initial_request_stream_start_timeout_ms: options.stream_start_timeout_ms,
                },
            )
            .await;
            return;
        }

        self.continue_run(options).await;
    }

    /// Continue from the current transcript. The last message must be a user or tool-result message.
    /// Queue deferral and timeout overrides apply only to the first provider request; later requests in
    /// the same run and later runs use the configured Agent defaults.
    pub async fn continue_run(&self, options: AgentContinuationOptions) {
        if lock(&self.inner.active_run).is_some() {
            panic!("Agent is already processing. Wait for completion before continuing.");
        }

        let last_message = lock(&self.inner.state).messages().last().cloned();
        let Some(last_message) = last_message else {
            panic!("No messages to continue from");
        };

        if last_message.role() == "assistant" {
            if lock(&self.inner.steering_queue).has_items() {
                self.run_queued_messages(
                    PendingQueue::Steering,
                    PromptRunOptions {
                        skip_initial_steering_poll: true,
                        initial_request_timeout_ms: options.timeout_ms,
                        initial_request_stream_start_timeout_ms: options.stream_start_timeout_ms,
                    },
                )
                .await;
                return;
            }

            if lock(&self.inner.follow_up_queue).has_items() {
                self.run_queued_messages(
                    PendingQueue::FollowUp,
                    PromptRunOptions {
                        skip_initial_steering_poll: false,
                        initial_request_timeout_ms: options.timeout_ms,
                        initial_request_stream_start_timeout_ms: options.stream_start_timeout_ms,
                    },
                )
                .await;
                return;
            }

            panic!("Cannot continue from message role: assistant");
        }

        self.run_continuation(options).await;
    }

    fn normalize_prompt_input(&self, input: AgentPromptInput) -> Vec<AgentMessage> {
        match input {
            AgentPromptInput::Messages(messages) => messages,
            AgentPromptInput::Message(message) => vec![message],
            AgentPromptInput::Text { text, images } => {
                let mut content = vec![ContentBlock::text(text)];
                content.extend(images.into_iter().map(ContentBlock::Image));
                vec![AgentMessage::Llm(Message::User(UserMessage {
                    content: UserContent::Blocks(content),
                    timestamp: now_ms(),
                }))]
            }
        }
    }

    async fn run_prompt_messages(&self, messages: Vec<AgentMessage>, options: PromptRunOptions) {
        self.run_with_lifecycle(|signal| {
            let messages = messages.clone();
            async move {
                let context = self.create_context_snapshot();
                let config = self.create_loop_config(options);
                run_agent_loop(
                    messages,
                    context,
                    config,
                    agent_event_sink(self.clone()),
                    signal,
                    Some(self.stream_function()),
                )
                .await;
            }
        })
        .await;
    }

    async fn run_queued_messages(&self, queue: PendingQueue, options: PromptRunOptions) {
        self.run_with_lifecycle(|signal| async move {
            Box::pin(self.run_claimed_queue(queue, options, signal)).await;
        })
        .await;
    }

    async fn run_claimed_queue(
        &self,
        queue: PendingQueue,
        options: PromptRunOptions,
        signal: Option<AbortSignal>,
    ) {
        let config = self.create_loop_config(options);
        let messages = match self.take_queued_messages(queue).await {
            Ok(messages) => messages,
            Err(error) => {
                crate::agent_loop::emit_run_failure(
                    &config,
                    error.0,
                    true,
                    &agent_event_sink(self.clone()),
                )
                .await;
                return;
            }
        };
        if messages.is_empty() {
            return;
        }
        Box::pin(run_agent_loop(
            messages,
            self.create_context_snapshot(),
            config,
            agent_event_sink(self.clone()),
            signal,
            Some(self.stream_function()),
        ))
        .await;
    }

    async fn run_continuation(&self, options: AgentContinuationOptions) {
        self.run_with_lifecycle(|signal| async move {
            let context = self.create_context_snapshot();
            let config = self.create_loop_config(PromptRunOptions {
                skip_initial_steering_poll: options.defer_queued_messages == Some(true),
                initial_request_timeout_ms: options.timeout_ms,
                initial_request_stream_start_timeout_ms: options.stream_start_timeout_ms,
            });
            run_agent_loop_continue(
                context,
                config,
                agent_event_sink(self.clone()),
                signal,
                Some(self.stream_function()),
            )
            .await;
        })
        .await;
    }

    fn create_context_snapshot(&self) -> AgentContext {
        let state = lock(&self.inner.state);
        AgentContext {
            system_prompt: state.system_prompt.clone(),
            messages: state.messages().to_vec(),
            tools: Some(state.tools().to_vec()),
            declared_tools: state.declared_tools.clone(),
        }
    }

    fn create_loop_config(&self, options: PromptRunOptions) -> AgentLoopConfig {
        let settings = lock(&self.inner.settings);
        let state = lock(&self.inner.state);

        let reasoning = match state.reasoning_baseline.as_deref() {
            Some(baseline) => parse_thinking_level(baseline),
            None => model_thinking_level_to_reasoning(state.thinking_level),
        };

        let mut config = AgentLoopConfig::new(state.model.clone(), settings.convert_to_llm.clone());
        config.options = SimpleStreamOptions {
            stream: maho_ai::types::StreamOptions {
                request: ProviderRequestOptions {
                    timeout_ms: settings.timeout_ms,
                    max_retry_delay_ms: settings.max_retry_delay_ms,
                    on_payload: settings.on_payload.clone(),
                    on_response: settings.on_response.clone(),
                    abort_server_side_fallback: settings.abort_server_side_fallback,
                    ..ProviderRequestOptions::default()
                },
                session_id: settings.session_id.clone(),
                transport: settings.transport,
                ..maho_ai::types::StreamOptions::default()
            },
            reasoning,
            thinking_selection: state.thinking_selection.clone(),
            thinking_budgets: settings.thinking_budgets.clone(),
            ..SimpleStreamOptions::default()
        };
        config.transform_context = settings.transform_context.clone();
        config.get_api_key = settings.get_api_key.clone();
        config.stream_start_timeout_ms = settings.stream_start_timeout_ms;
        config.initial_request_timeout_ms = options.initial_request_timeout_ms;
        config.initial_request_stream_start_timeout_ms =
            options.initial_request_stream_start_timeout_ms;
        config.tool_execution = Some(settings.tool_execution);
        config.removed_tool_hints = Some(settings.removed_tool_hints.clone());
        config.resolve_unknown_tool_call = settings.resolve_unknown_tool_call.clone();
        config.before_tool_call = settings.before_tool_call.clone();
        config.after_tool_call = settings.after_tool_call.clone();
        config.cursor_exec_handlers = settings.cursor_exec_handlers.clone();
        config.finish_turn = settings.finish_turn.clone();
        config.prepare_request = settings.prepare_request.clone();

        if let Some(should_stop_after_turn) = settings.should_stop_after_turn.clone() {
            let agent = self.clone();
            config.should_stop_after_turn = Some(Arc::new(move |context| {
                let signal = agent.signal();
                should_stop_after_turn(context, signal)
            }));
        }

        if settings.prepare_next_turn_with_context.is_some() || settings.prepare_next_turn.is_some()
        {
            let agent = self.clone();
            let with_context = settings.prepare_next_turn_with_context.clone();
            let without_context = settings.prepare_next_turn.clone();
            config.prepare_next_turn = Some(Arc::new(move |context| {
                let signal = agent.signal();
                if let Some(with_context) = with_context.clone() {
                    return with_context(context, signal);
                }
                match without_context.clone() {
                    Some(without_context) => without_context(signal),
                    None => Box::pin(async { None }),
                }
            }));
        }

        let skip_initial_steering_poll =
            Arc::new(AtomicBool::new(options.skip_initial_steering_poll));
        let steering_generation = Arc::new(Mutex::new(
            lock(&self.inner.steering_queue).clear_generation(),
        ));
        let follow_up_generation = Arc::new(Mutex::new(
            lock(&self.inner.follow_up_queue).clear_generation(),
        ));
        let steering_generation_for_restore = steering_generation.clone();
        let follow_up_generation_for_restore = follow_up_generation.clone();

        let port = settings.queued_input_port.clone();
        let steering_port = port.clone();
        let steering_agent = self.clone();
        config.get_steering_messages = Some(Arc::new(move || {
            if skip_initial_steering_poll.swap(false, Ordering::SeqCst) {
                return Box::pin(async { Vec::new() });
            }
            let agent = steering_agent.clone();
            let generation = steering_generation.clone();
            let drain: QueuedDrain = Arc::new(move || {
                let mut queue = lock(&agent.inner.steering_queue);
                *lock(&generation) = queue.clear_generation();
                queue.drain()
            });
            match &steering_port {
                Some(port) => port.claim(PendingQueue::Steering, drain),
                None => {
                    let messages = drain();
                    Box::pin(async move { messages })
                }
            }
        }));

        let follow_up_port = port.clone();
        let follow_up_agent = self.clone();
        config.get_follow_up_messages = Some(Arc::new(move || {
            let agent = follow_up_agent.clone();
            let generation = follow_up_generation.clone();
            let drain: QueuedDrain = Arc::new(move || {
                let mut queue = lock(&agent.inner.follow_up_queue);
                *lock(&generation) = queue.clear_generation();
                queue.drain()
            });
            match &follow_up_port {
                Some(port) => port.claim(PendingQueue::FollowUp, drain),
                None => {
                    let messages = drain();
                    Box::pin(async move { messages })
                }
            }
        }));

        if let Some(port) = port.clone() {
            config.commit_queued_consumption = Some(Arc::new(move |queue, messages| {
                port.commit_consumption(queue, messages)
            }));
        }

        let restore_agent = self.clone();
        config.restore_pending_messages = Some(Arc::new(move |queue, messages| {
            let generation = match queue {
                PendingQueue::Steering => *lock(&steering_generation_for_restore),
                PendingQueue::FollowUp => *lock(&follow_up_generation_for_restore),
            };
            let give_back = restore_agent.give_back(queue, messages.clone(), generation);
            match &port {
                Some(port) => port.release(queue, messages, give_back),
                None => {
                    (give_back.restore)();
                    Box::pin(async {})
                }
            }
        }));

        config
    }

    fn pending_queue(&self, queue: PendingQueue) -> &Mutex<PendingMessageQueue> {
        match queue {
            PendingQueue::Steering => &self.inner.steering_queue,
            PendingQueue::FollowUp => &self.inner.follow_up_queue,
        }
    }

    fn give_back(
        &self,
        queue: PendingQueue,
        messages: Vec<AgentMessage>,
        generation: u64,
    ) -> QueuedGiveBack {
        let check_agent = self.clone();
        let restore_agent = self.clone();
        QueuedGiveBack {
            restorable: Arc::new(move || {
                lock(check_agent.pending_queue(queue)).clear_generation() == generation
            }),
            restore: Arc::new(move || {
                let mut pending = lock(restore_agent.pending_queue(queue));
                if pending.clear_generation() != generation {
                    return false;
                }
                pending.prepend(messages.clone());
                true
            }),
        }
    }

    async fn take_queued_messages(
        &self,
        queue: PendingQueue,
    ) -> Result<Vec<AgentMessage>, crate::local_failure::LocalFailure> {
        let port = lock(&self.inner.settings)
            .queued_input_port
            .clone()
            .map(crate::local_failure::local_failure_port);
        let Some(port) = port else {
            return Ok(lock(self.pending_queue(queue)).drain());
        };
        let generation = Arc::new(Mutex::new(
            lock(self.pending_queue(queue)).clear_generation(),
        ));
        let drain_generation = generation.clone();
        let agent = self.clone();
        let messages = port
            .claim(
                queue,
                Arc::new(move || {
                    let mut pending = lock(agent.pending_queue(queue));
                    *lock(&drain_generation) = pending.clear_generation();
                    pending.drain()
                }),
            )
            .await;
        if !messages.is_empty()
            && let Err(error) = port.commit_local(queue, messages.clone()).await
        {
            let claimed_generation = *lock(&generation);
            port.release(
                queue,
                messages.clone(),
                self.give_back(queue, messages, claimed_generation),
            )
            .await;
            return Err(error);
        }
        Ok(messages)
    }

    pub fn set_finish_turn(&self, callback: Option<crate::types::FinishTurn>) {
        lock(&self.inner.settings).finish_turn = callback;
    }

    pub fn set_prepare_request(&self, callback: Option<crate::types::PrepareRequest>) {
        lock(&self.inner.settings).prepare_request = callback;
    }

    pub fn set_queued_input_port(&self, port: Option<Arc<dyn QueuedInputPort>>) {
        lock(&self.inner.settings).queued_input_port = port;
    }

    async fn run_with_lifecycle<F, Fut>(&self, executor: F)
    where
        F: FnOnce(Option<AbortSignal>) -> Fut,
        Fut: std::future::Future<Output = ()>,
    {
        if lock(&self.inner.active_run).is_some() {
            panic!("Agent is already processing.");
        }

        let abort_controller = AbortController::new();
        let (completion, _receiver) = tokio::sync::watch::channel(false);
        *lock(&self.inner.active_run) = Some(ActiveRun {
            completion,
            abort_controller: abort_controller.clone(),
            suppress_queued_message_drain: false,
        });

        {
            let mut state = lock(&self.inner.state);
            state.is_streaming = true;
            state.streaming_message = None;
            state.error_message = None;
            state.provider_diagnostic = None;
        }

        executor(Some(abort_controller.signal())).await;

        // A provider-returned terminal error/abort has no safe implicit owner for
        // queued work. Park it until an external retry/compaction owner continues,
        // or until a later admitted prompt drains it through the normal queue poll.
        loop {
            let run_continues = {
                let active_run = lock(&self.inner.active_run);
                match active_run.as_ref() {
                    Some(run) => {
                        !abort_controller.signal().aborted() && !run.suppress_queued_message_drain
                    }
                    None => false,
                }
            };
            if !run_continues
                || !self.can_drain_queued_messages_after_run()
                || !self.has_queued_messages()
            {
                break;
            }
            self.run_queued_messages_after_agent_end(Some(abort_controller.signal()))
                .await;
        }

        self.finish_run();
    }

    fn can_drain_queued_messages_after_run(&self) -> bool {
        let state = lock(&self.inner.state);
        let Some(last_message) = state.messages().last() else {
            return true;
        };
        match last_message.as_assistant() {
            Some(assistant) => {
                assistant.stop_reason != maho_ai::types::StopReason::Error
                    && assistant.stop_reason != maho_ai::types::StopReason::Aborted
            }
            None => true,
        }
    }

    async fn run_queued_messages_after_agent_end(&self, signal: Option<AbortSignal>) {
        if lock(&self.inner.steering_queue).has_items() {
            self.run_claimed_queue(
                PendingQueue::Steering,
                PromptRunOptions {
                    skip_initial_steering_poll: true,
                    initial_request_timeout_ms: None,
                    initial_request_stream_start_timeout_ms: None,
                },
                signal,
            )
            .await;
            return;
        }
        if lock(&self.inner.follow_up_queue).has_items() {
            self.run_claimed_queue(PendingQueue::FollowUp, PromptRunOptions::default(), signal)
                .await;
        }
    }

    fn finish_run(&self) {
        {
            let mut state = lock(&self.inner.state);
            state.is_streaming = false;
            state.streaming_message = None;
            state.pending_tool_calls.clear();
        }
        let mut active_run = lock(&self.inner.active_run);
        if let Some(run) = active_run.as_ref() {
            let _ = run.completion.send(true);
        }
        *active_run = None;
    }

    /// Reduce internal state for a loop event, then await listeners.
    ///
    /// `agent_end` only means no further loop events will be emitted. The run is
    /// considered idle later, after all awaited listeners for `agent_end` finish
    /// and `finishRun()` clears runtime-owned state.
    async fn process_events(&self, event: AgentEvent) {
        match &event {
            AgentEvent::MessageStart { message } => {
                lock(&self.inner.state).streaming_message = Some(message.clone());
            }
            AgentEvent::MessageUpdate { message, .. } => {
                lock(&self.inner.state).streaming_message = Some(message.clone());
            }
            AgentEvent::MessageEnd { message } => {
                let mut state = lock(&self.inner.state);
                state.streaming_message = None;
                state.push_message(message.clone());
            }
            AgentEvent::ToolExecutionStart { tool_call_id, .. } => {
                lock(&self.inner.state)
                    .pending_tool_calls
                    .insert(tool_call_id.clone());
            }
            AgentEvent::ToolExecutionEnd { tool_call_id, .. } => {
                lock(&self.inner.state)
                    .pending_tool_calls
                    .remove(tool_call_id);
            }
            AgentEvent::TurnEnd { message, .. } => {
                if let Some(assistant) = message.as_assistant()
                    && let Some(error_message) = assistant.error_message.clone()
                    && !error_message.is_empty()
                {
                    let mut state = lock(&self.inner.state);
                    state.error_message = Some(error_message);
                    state.provider_diagnostic = assistant
                        .provider_diagnostic
                        .as_ref()
                        .and_then(maho_ai::types::sanitize_provider_diagnostic);
                }
            }
            AgentEvent::AgentEnd { .. } => {
                lock(&self.inner.state).streaming_message = None;
            }
            _ => {}
        }

        let Some(signal) = self.signal() else {
            // TS throws "Agent listener invoked outside active run"; the Rust event sink has no
            // error channel, and no loop event is ever emitted outside a run.
            return;
        };
        let listeners: Vec<AgentListener> = lock(&self.inner.listeners)
            .entries
            .iter()
            .map(|(_, listener)| listener.clone())
            .collect();
        let last = listeners.len().saturating_sub(1);
        let mut owned = Some(event);
        for (index, listener) in listeners.iter().enumerate() {
            let next = if index == last {
                owned.take()
            } else {
                owned.clone()
            };
            if let Some(next) = next {
                listener(next, signal.clone()).await;
            }
        }
    }

    /// Emit a host-generated event through the normal listener pipeline.
    ///
    /// Used by the Cursor exec bridge: bridge-run tools execute inside the
    /// provider stream, outside the loop's executor, so their
    /// `tool_execution_start`/`tool_execution_end` lifecycle must be injected
    /// here or the live tool card for a synthesized call never resolves.
    /// A bridge execution may settle after an aborted run has already ended; its
    /// late lifecycle event belongs to that finished run and must be discarded.
    pub async fn emit_external_event(&self, event: AgentEvent, run_signal: Option<AbortSignal>) {
        let active_signal = self.signal();
        let Some(active_signal) = active_signal else {
            return;
        };
        if run_signal.is_some_and(|signal| !signal.same_signal(&active_signal)) {
            return;
        }
        self.process_events(event).await;
    }

    pub async fn build_provider_context(
        &self,
        context: &AgentContext,
        signal: Option<AbortSignal>,
    ) -> maho_ai::types::Context {
        let config = self.create_loop_config(PromptRunOptions::default());
        build_provider_context(context, &config, signal.as_ref()).await
    }

    /// `Agent.streamFunction`.
    pub fn stream_function(&self) -> StreamFn {
        lock(&self.inner.settings).stream_function.clone()
    }

    /// `Agent.streamFunction` (public mutable field in TS).
    pub fn set_stream_function(&self, stream_fn: StreamFn) {
        lock(&self.inner.settings).stream_function = stream_fn;
    }

    /// `Agent.convertToLlm`.
    pub fn set_convert_to_llm(&self, convert_to_llm: ConvertToLlm) {
        lock(&self.inner.settings).convert_to_llm = convert_to_llm;
    }

    /// `Agent.convertToLlm` (read side of the public TS field).
    pub fn convert_to_llm(&self) -> ConvertToLlm {
        lock(&self.inner.settings).convert_to_llm.clone()
    }

    /// `Agent.transformContext` (read side of the public TS field).
    pub fn transform_context(&self) -> Option<TransformContext> {
        lock(&self.inner.settings).transform_context.clone()
    }

    /// `Agent.onPayload` (read side of the public TS field).
    pub fn on_payload(&self) -> Option<OnPayload> {
        lock(&self.inner.settings).on_payload.clone()
    }

    /// `Agent.onResponse` (read side of the public TS field).
    pub fn on_response(&self) -> Option<OnResponse> {
        lock(&self.inner.settings).on_response.clone()
    }

    /// `Agent.transformContext`.
    pub fn set_transform_context(&self, transform_context: Option<TransformContext>) {
        lock(&self.inner.settings).transform_context = transform_context;
    }

    /// `Agent.getApiKey`.
    pub fn set_get_api_key(&self, get_api_key: Option<GetApiKey>) {
        lock(&self.inner.settings).get_api_key = get_api_key;
    }

    /// `Agent.beforeToolCall`.
    pub fn set_before_tool_call(&self, before_tool_call: Option<BeforeToolCall>) {
        lock(&self.inner.settings).before_tool_call = before_tool_call;
    }

    /// `Agent.afterToolCall`.
    pub fn set_after_tool_call(&self, after_tool_call: Option<AfterToolCall>) {
        lock(&self.inner.settings).after_tool_call = after_tool_call;
    }

    /// `Agent.shouldStopAfterTurn`.
    pub fn set_should_stop_after_turn(&self, callback: Option<ShouldStopAfterTurnWithSignal>) {
        lock(&self.inner.settings).should_stop_after_turn = callback;
    }

    /// `Agent.prepareNextTurn` (getter).
    pub fn prepare_next_turn(&self) -> Option<PrepareNextTurnWithoutContext> {
        lock(&self.inner.settings).prepare_next_turn.clone()
    }

    /// `Agent.prepareNextTurnWithContext` (getter).
    pub fn prepare_next_turn_with_context(&self) -> Option<PrepareNextTurnWithContext> {
        lock(&self.inner.settings)
            .prepare_next_turn_with_context
            .clone()
    }

    /// `Agent.prepareNextTurn`.
    pub fn set_prepare_next_turn(&self, callback: Option<PrepareNextTurnWithoutContext>) {
        lock(&self.inner.settings).prepare_next_turn = callback;
    }

    /// `Agent.prepareNextTurnWithContext`.
    pub fn set_prepare_next_turn_with_context(&self, callback: Option<PrepareNextTurnWithContext>) {
        lock(&self.inner.settings).prepare_next_turn_with_context = callback;
    }

    /// `Agent.sessionId`.
    pub fn session_id(&self) -> Option<String> {
        lock(&self.inner.settings).session_id.clone()
    }

    pub fn set_session_id(&self, session_id: Option<String>) {
        lock(&self.inner.settings).session_id = session_id;
    }

    /// `Agent.thinkingBudgets`.
    pub fn thinking_budgets(&self) -> Option<ThinkingBudgets> {
        lock(&self.inner.settings).thinking_budgets.clone()
    }

    pub fn set_thinking_budgets(&self, thinking_budgets: Option<ThinkingBudgets>) {
        lock(&self.inner.settings).thinking_budgets = thinking_budgets;
    }

    /// `Agent.transport`.
    pub fn transport(&self) -> Option<Transport> {
        lock(&self.inner.settings).transport
    }

    pub fn set_transport(&self, transport: Option<Transport>) {
        lock(&self.inner.settings).transport = transport;
    }

    /// `Agent.timeoutMs`.
    pub fn timeout_ms(&self) -> Option<u64> {
        lock(&self.inner.settings).timeout_ms
    }

    pub fn set_timeout_ms(&self, timeout_ms: Option<u64>) {
        lock(&self.inner.settings).timeout_ms = timeout_ms;
    }

    /// `Agent.streamStartTimeoutMs`.
    pub fn stream_start_timeout_ms(&self) -> Option<u64> {
        lock(&self.inner.settings).stream_start_timeout_ms
    }

    pub fn set_stream_start_timeout_ms(&self, timeout_ms: Option<u64>) {
        lock(&self.inner.settings).stream_start_timeout_ms = timeout_ms;
    }

    /// `Agent.maxRetryDelayMs`.
    pub fn max_retry_delay_ms(&self) -> Option<u64> {
        lock(&self.inner.settings).max_retry_delay_ms
    }

    pub fn set_max_retry_delay_ms(&self, max_retry_delay_ms: Option<u64>) {
        lock(&self.inner.settings).max_retry_delay_ms = max_retry_delay_ms;
    }

    /// `Agent.toolExecution`.
    pub fn tool_execution(&self) -> ToolExecutionMode {
        lock(&self.inner.settings).tool_execution
    }

    pub fn set_tool_execution(&self, tool_execution: ToolExecutionMode) {
        lock(&self.inner.settings).tool_execution = tool_execution;
    }

    /// `Agent.removedToolHints`.
    pub fn removed_tool_hints(&self) -> BTreeMap<String, String> {
        lock(&self.inner.settings).removed_tool_hints.clone()
    }

    pub fn set_removed_tool_hints(&self, hints: BTreeMap<String, String>) {
        lock(&self.inner.settings).removed_tool_hints = hints;
    }

    /// `Agent.resolveUnknownToolCall`.
    pub fn set_resolve_unknown_tool_call(&self, resolver: Option<ResolveUnknownToolCall>) {
        lock(&self.inner.settings).resolve_unknown_tool_call = resolver;
    }

    /// `Agent.abortServerSideFallback`.
    pub fn abort_server_side_fallback(&self) -> Option<bool> {
        lock(&self.inner.settings).abort_server_side_fallback
    }

    pub fn set_abort_server_side_fallback(&self, abort_server_side_fallback: Option<bool>) {
        lock(&self.inner.settings).abort_server_side_fallback = abort_server_side_fallback;
    }

    /// `Agent.cursorExecHandlers`.
    pub fn set_cursor_exec_handlers(&self, handlers: Option<CursorExecHandlersConfig>) {
        lock(&self.inner.settings).cursor_exec_handlers = handlers;
    }

    /// `onPayload`, forwarded to the stream function.
    pub fn set_on_payload(&self, on_payload: Option<OnPayload>) {
        lock(&self.inner.settings).on_payload = on_payload;
    }

    /// `onResponse`, forwarded to the stream function.
    pub fn set_on_response(&self, on_response: Option<OnResponse>) {
        lock(&self.inner.settings).on_response = on_response;
    }
}

/// `Agent.prompt(input)` accepts text (+images), a single message, or a batch.
///
/// The union mirrors the TS overloads; the variants differ in size only because a batch carries a
/// `Vec`, so the enum keeps the TS shape instead of boxing one arm.
#[allow(clippy::large_enum_variant)]
#[derive(Clone)]
pub enum AgentPromptInput {
    Text {
        text: String,
        images: Vec<ImageContent>,
    },
    Message(AgentMessage),
    Messages(Vec<AgentMessage>),
}

impl AgentPromptInput {
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text {
            text: text.into(),
            images: Vec::new(),
        }
    }
}

#[derive(Clone, Copy, Default)]
struct PromptRunOptions {
    skip_initial_steering_poll: bool,
    initial_request_timeout_ms: Option<u64>,
    initial_request_stream_start_timeout_ms: Option<u64>,
}

/// Unused marker so the empty-usage helper stays part of the ported surface.
pub fn empty_failure_usage() -> Usage {
    empty_usage()
}

/// An `AgentEvent` sink that forwards into an `Agent`'s listener pipeline.
pub fn agent_event_sink(agent: Agent) -> AgentEventSink {
    Arc::new(move |event| {
        let agent = agent.clone();
        Box::pin(async move { agent.process_events(event).await })
    })
}

#[allow(dead_code)]
fn unused_marker(
    _: &AtomicU64,
    _: &AfterToolCallContext,
    _: &AfterToolCallResult,
    _: &BeforeToolCallContext,
    _: &BeforeToolCallResult,
) {
}

#[cfg(test)]
mod queue_ownership_tests {
    use super::*;

    fn options() -> AgentOptions {
        AgentOptions {
            stream_fn: Some(Arc::new(|_, _, _| {
                panic!("queue ownership tests must not request a provider stream")
            })),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn provider_diagnostic_follows_error_projection_and_reset_lifecycle() {
        // Given: a terminal provider failure with structured quota evidence.
        let agent = Agent::new(options());
        let diagnostic = maho_ai::types::ProviderDiagnostic {
            resets_at: None,
            category: maho_ai::types::ProviderDiagnosticCategory::Quota,
            http_status: Some(429),
            code: Some("insufficient_quota".into()),
            evidence: maho_ai::types::ProviderDiagnosticEvidence::StructuredCode,
        };
        let message = crate::assistant_terminal_state::create_terminal_failure_assistant_message(
            &default_model(),
            maho_ai::types::StopReason::Error,
            &crate::assistant_terminal_state::AgentStreamError::ProviderFailure {
                message: "failure".into(),
                diagnostic: diagnostic.clone(),
            },
            None,
        );
        // When: the terminal event is projected into public state.
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            agent.process_events(AgentEvent::TurnEnd {
                message: maho_ai::types::Message::Assistant(Box::new(message)).into(),
                tool_results: Vec::new(),
            }),
        )
        .await
        .expect("bounded event projection");
        // Then: the structured cause is public and reset removes it with the error.
        assert_eq!(agent.state().provider_diagnostic, Some(diagnostic));
        agent.reset();
        assert!(agent.state().provider_diagnostic.is_none());
        assert!(agent.state().error_message.is_none());
    }

    struct RejectConsumption;

    impl QueuedInputPort for RejectConsumption {
        fn claim(
            &self,
            _: PendingQueue,
            drain: QueuedDrain,
        ) -> maho_ai::types::BoxFuture<'static, Vec<AgentMessage>> {
            Box::pin(async move { drain() })
        }

        fn release(
            &self,
            _: PendingQueue,
            _: Vec<AgentMessage>,
            give_back: QueuedGiveBack,
        ) -> maho_ai::types::BoxFuture<'static, ()> {
            Box::pin(async move {
                (give_back.restore)();
            })
        }

        fn commit_consumption(
            &self,
            _: PendingQueue,
            _: Vec<AgentMessage>,
        ) -> maho_ai::types::BoxFuture<'static, Result<(), String>> {
            Box::pin(async { Err("not durable".into()) })
        }
    }

    #[tokio::test]
    async fn queue_first_continuation_restores_rejected_consumption() {
        // Given: a queue owner that refuses durable consumption.
        let agent = Agent::new(AgentOptions {
            queued_input_port: Some(Arc::new(RejectConsumption)),
            ..options()
        });
        let message = agent
            .normalize_prompt_input(AgentPromptInput::text("queued"))
            .remove(0);
        agent.steer(message);
        // When: continuation tries to admit that queue as its initial prompt.
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            agent.continue_with_queued_messages(AgentContinuationOptions::default()),
        )
        .await
        .expect("bounded continuation");
        // Then: input is restored, never emitted, and the run settles.
        assert!(agent.has_queued_messages());
        assert!(
            !agent
                .state()
                .messages()
                .iter()
                .any(|message| message.role() == "user")
        );
        assert!(!agent.is_streaming());
    }

    #[test]
    fn clear_after_claim_prevents_give_back() {
        // Given: a give-back token from the current queue generation.
        let agent = Agent::new(options());
        let messages = agent.normalize_prompt_input(AgentPromptInput::text("claimed"));
        let generation = lock(&agent.inner.steering_queue).clear_generation();
        let give_back = agent.give_back(PendingQueue::Steering, messages, generation);
        // When: the owner clears the queue after the claim.
        agent.clear_steering_queue();
        // Then: both admission checks reject restoration.
        assert!(!(give_back.restorable)());
        assert!(!(give_back.restore)());
        assert!(!agent.has_queued_messages());
    }
}
