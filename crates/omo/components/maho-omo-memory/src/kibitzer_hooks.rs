//! The five Kibitzer hook registrations behind a sink (latest `kibitzer/hooks.ts`).
//!
//! Every sink call is synchronous inside the handler: the host disposes the context when the
//! handler returns. `agent_settled` never wakes; it refreshes the sink's bookkeeping.

use std::sync::Arc;

use serde_json::Value;

use crate::kibitzer_delivery::KibitzerToolResultGate;
use crate::kibitzer_events::newest_assistant_text;
use crate::recall_session_read::{RECALL_TEXT_WINDOW, judge_transcript, user_texts};
use crate::recall_transcript_mentions::RECALL_PATH_ENTRY_WINDOW;

pub type KibitzerGateResolver = Arc<dyn Fn(&maho_ext_api::ExtensionContext) -> KibitzerToolResultGate + Send + Sync>;

pub fn default_gate_resolver() -> KibitzerGateResolver {
    Arc::new(|context| KibitzerToolResultGate {
        has_pending_messages: context.has_pending_messages().unwrap_or(false),
        is_idle: context.is_idle(),
    })
}

/// One `tool_result` event's tool identity and reported result, captured while the ctx is alive.
pub struct KibitzerToolResultEvent<'a> {
    pub tool_call_id: &'a str,
    pub tool_name: &'a str,
    pub input: &'a Value,
    pub content: &'a [maho_ext_api::ToolContent],
    pub is_error: bool,
}

/// The newest suffix of the branch snapshot a tool hook captured, plus the TRUE branch length
/// (the events cursor). `entries` is a suffix of `get_branch()`'s `data`, long enough that every
/// tool-hook consumer reads the same result it would from the full branch.
pub struct BranchTail<'a> {
    pub len: usize,
    pub entries: &'a [Value],
}

/// The branch tail the tool hooks capture: at least the mention window
/// ([`RECALL_PATH_ENTRY_WINDOW`]), extended back until the newest assistant text
/// (`newest_assistant_text`), [`RECALL_TEXT_WINDOW`] planner user texts (`user_texts`) and
/// [`RECALL_TEXT_WINDOW`] judge turns (`judge_transcript`) are all inside it, so each consumer's
/// newest-first scan stops within the tail exactly where it would stop on the full branch.
pub fn tool_branch_tail(session_manager: &dyn maho_ext_api::SessionManager) -> (usize, Vec<Value>) {
    let mut assistant = false;
    let mut users = 0usize;
    let mut turns = 0usize;
    let mut satisfied = |entry: &Value| {
        let one = std::slice::from_ref(entry);
        assistant = assistant || newest_assistant_text(one).is_some();
        users += user_texts(one).len();
        turns += judge_transcript(one).len();
        assistant && users >= RECALL_TEXT_WINDOW && turns >= RECALL_TEXT_WINDOW
    };
    let (len, tail) = session_manager.get_branch_tail(RECALL_PATH_ENTRY_WINDOW, &mut satisfied);
    (len, tail.into_iter().map(|entry| entry.data).collect())
}

/// The synchronous capture surface the composition implements.
///
/// `entries` is the REAL branch snapshot (`context.session_manager.get_branch()`'s `data`), captured
/// while the ctx is still alive. The consumer derives the events cursor from its LENGTH (the branch
/// length), never from a synthetic monotonic counter. The tool hooks receive a [`BranchTail`]: the
/// newest suffix of that snapshot plus its true length. `prompt` is the `before_agent_start` event's
/// OWN prompt, passed so it is never inferred from a stale transcript.
pub trait KibitzerHookSink: Send + Sync {
    fn on_before_agent_start(&self, session_id: &str, prompt: &str, entries: &[Value]);
    fn on_tool_call(&self, session_id: &str, tool_call_id: &str, tool_name: &str, input: &Value, branch: &BranchTail<'_>);
    fn on_tool_result(&self, session_id: &str, event: &KibitzerToolResultEvent<'_>, branch: &BranchTail<'_>, gate: &KibitzerToolResultGate);
    fn on_turn_end(&self, session_id: &str);
    fn on_agent_settled(&self, session_id: &str);
    fn on_session_shutdown(&self, session_id: &str);
    fn on_compaction_accepted(&self, session_id: &str);
    /// Whether the next `kind` hook for `session_id` reads its `entries`; `false` skips cloning the
    /// branch (the sink then receives an empty slice it never reads).
    fn wants_branch(&self, _kind: maho_ext_api::EventKind, _session_id: &str) -> bool {
        true
    }
}

pub fn register_kibitzer_hooks(api: &mut maho_ext_api::ExtensionApi, sink: Arc<dyn KibitzerHookSink>, gate_resolver: KibitzerGateResolver) {
    for kind in [
        maho_ext_api::EventKind::BeforeAgentStart,
        maho_ext_api::EventKind::ToolCall,
        maho_ext_api::EventKind::ToolResult,
        maho_ext_api::EventKind::TurnEnd,
        maho_ext_api::EventKind::AgentSettled,
        maho_ext_api::EventKind::SessionShutdown,
        maho_ext_api::EventKind::SessionCompact,
    ] {
        let sink = sink.clone();
        let gate_resolver = gate_resolver.clone();
        api.on(kind, Arc::new(move |event, context| {
            let sink = sink.clone();
            let session_id = context.session_manager.session_id();
            // Capture the REAL branch snapshot synchronously, while the ctx is alive: the host
            // disposes the context when the handler returns. The consumer derives the events cursor
            // from this branch LENGTH - never a synthetic monotonic counter.
            let entries: Vec<Value> = if kind == maho_ext_api::EventKind::BeforeAgentStart && sink.wants_branch(kind, session_id) {
                context.session_manager.get_branch().into_iter().map(|entry| entry.data).collect()
            } else {
                Vec::new()
            };
            // The tool hooks fire per tool call: capture only the branch TAIL every consumer reads
            // (see `tool_branch_tail`) plus the TRUE branch length as the cursor.
            let (branch_len, tail): (usize, Vec<Value>) = if (kind == maho_ext_api::EventKind::ToolCall || kind == maho_ext_api::EventKind::ToolResult)
                && sink.wants_branch(kind, session_id)
            {
                tool_branch_tail(context.session_manager.as_ref())
            } else {
                (entries.len(), Vec::new())
            };
            // The `before_agent_start` event's OWN prompt, captured from the event variant (not the
            // branch) so it is never inferred from a stale transcript.
            let prompt: Option<String> = match event {
                maho_ext_api::ExtensionEvent::BeforeAgentStart(payload) => Some(payload.prompt.clone()),
                _ => None,
            };
            let captured = match event {
                maho_ext_api::ExtensionEvent::ToolCall(call) => Some((call.tool_call_id.clone(), call.tool_name.clone(), call.input.clone(), Vec::new(), false)),
                maho_ext_api::ExtensionEvent::ToolResult(result) => Some((result.tool_call_id.clone(), result.tool_name.clone(), result.input.clone(), result.content.clone(), result.is_error)),
                _ => None,
            };
            let gate = if kind == maho_ext_api::EventKind::ToolResult { Some(gate_resolver(context)) } else { None };
            Box::pin(async move {
                match kind {
                    maho_ext_api::EventKind::BeforeAgentStart => sink.on_before_agent_start(session_id, prompt.as_deref().unwrap_or(""), &entries),
                    maho_ext_api::EventKind::ToolCall => {
                        if let Some((tool_call_id, tool_name, input, _, _)) = captured {
                            sink.on_tool_call(session_id, &tool_call_id, &tool_name, &input, &BranchTail { len: branch_len, entries: &tail });
                        }
                    }
                    maho_ext_api::EventKind::ToolResult => {
                        if let (Some((tool_call_id, tool_name, input, content, is_error)), Some(gate)) = (captured, gate) {
                            sink.on_tool_result(session_id, &KibitzerToolResultEvent { tool_call_id: &tool_call_id, tool_name: &tool_name, input: &input, content: &content, is_error }, &BranchTail { len: branch_len, entries: &tail }, &gate);
                        }
                    }
                    maho_ext_api::EventKind::TurnEnd => sink.on_turn_end(session_id),
                    maho_ext_api::EventKind::AgentSettled => sink.on_agent_settled(session_id),
                    maho_ext_api::EventKind::SessionShutdown => sink.on_session_shutdown(session_id),
                    maho_ext_api::EventKind::SessionCompact => sink.on_compaction_accepted(session_id),
                    _ => {}
                }
                Ok(maho_ext_api::EventResult::None)
            })
        }));
    }
}
