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

/// One retained entry's contribution to the [`tool_branch_tail`] stop condition.
#[derive(Clone, Copy)]
struct TailMark {
    assistant: bool,
    users: usize,
    turns: usize,
}

fn tail_mark(entry: &Value) -> TailMark {
    let one = std::slice::from_ref(entry);
    TailMark { assistant: newest_assistant_text(one).is_some(), users: user_texts(one).len(), turns: judge_transcript(one).len() }
}

/// Start of the suffix [`tool_branch_tail`] keeps, scanning `marks` newest-first. The kept start
/// only moves forward as entries are appended (every condition is a monotone count), so scanning the
/// previously kept tail plus the appended entries yields the full-branch result.
fn tail_start(marks: &[TailMark]) -> usize {
    let len = marks.len();
    let (mut assistant, mut users, mut turns, mut done) = (false, 0usize, 0usize, false);
    let mut start = len;
    while start > 0 && (len - start < RECALL_PATH_ENTRY_WINDOW || !done) {
        start -= 1;
        if !done {
            let mark = marks[start];
            assistant = assistant || mark.assistant;
            users += mark.users;
            turns += mark.turns;
            done = assistant && users >= RECALL_TEXT_WINDOW && turns >= RECALL_TEXT_WINDOW;
        }
    }
    start
}

fn entry_id(entry: &Value) -> Option<&str> {
    entry.get("id").and_then(Value::as_str)
}

/// The cached shape of a session's [`tool_branch_tail`]: the leaf and branch length it was taken
/// at plus one [`TailMark`] per kept entry. No entry payloads are retained; the hook re-reads the
/// kept suffix (`marks.len()` entries) on demand, so the cache stays small for long sessions.
struct CachedBranchTail {
    leaf_id: String,
    len: usize,
    marks: Vec<TailMark>,
}

/// Per-session [`tool_branch_tail`] bookkeeping, extended incrementally: a hook walks only the
/// entries appended since the cached leaf to find where the kept tail starts, instead of
/// re-scanning the branch back to the 6th-newest user prompt on every tool event. Any other branch
/// change (leaf moved off the cached chain, trimmed history) falls back to the full scan.
#[derive(Default)]
pub struct ToolBranchTailCache {
    sessions: std::sync::Mutex<std::collections::BTreeMap<String, CachedBranchTail>>,
}

impl ToolBranchTailCache {
    /// The same `(len, tail)` [`tool_branch_tail`] returns for the current branch.
    pub fn tail(&self, session_id: &str, session_manager: &dyn maho_ext_api::SessionManager) -> (usize, Arc<Vec<Value>>) {
        let cached = self.sessions.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(session_id);
        let (next, tail) = cached.and_then(|cached| Self::extend(cached, session_manager)).unwrap_or_else(|| Self::full(session_manager));
        let result = (next.len, Arc::new(tail));
        if !next.leaf_id.is_empty() {
            self.sessions.lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(session_id.to_owned(), next);
        }
        result
    }

    pub fn forget(&self, session_id: &str) {
        self.sessions.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(session_id);
    }

    fn full(session_manager: &dyn maho_ext_api::SessionManager) -> (CachedBranchTail, Vec<Value>) {
        let (len, tail) = tool_branch_tail(session_manager);
        let marks = tail.iter().map(tail_mark).collect();
        let leaf_id = tail.last().and_then(entry_id).unwrap_or_default().to_owned();
        (CachedBranchTail { leaf_id, len, marks }, tail)
    }

    fn extend(mut cached: CachedBranchTail, session_manager: &dyn maho_ext_api::SessionManager) -> Option<(CachedBranchTail, Vec<Value>)> {
        let (len, walked) = session_manager.get_branch_tail(0, &mut |entry| entry_id(entry) == Some(cached.leaf_id.as_str()));
        let mut walked = walked.into_iter();
        if entry_id(&walked.next()?.data) != Some(cached.leaf_id.as_str()) || len != cached.len + walked.len() {
            return None;
        }
        let appended: Vec<Value> = walked.map(|entry| entry.data).collect();
        cached.marks.extend(appended.iter().map(tail_mark));
        let start = tail_start(&cached.marks);
        cached.marks.drain(..start);
        let kept = cached.marks.len();
        let tail = if kept <= appended.len() {
            let mut appended = appended;
            appended.drain(..appended.len() - kept);
            appended
        } else {
            // The kept tail reaches back past the appended entries: re-read exactly that suffix.
            let (reread_len, reread) = session_manager.get_branch_tail(kept, &mut |_| true);
            let reread: Vec<Value> = reread.into_iter().map(|entry| entry.data).collect();
            let leaf = appended.last().and_then(entry_id).unwrap_or(cached.leaf_id.as_str());
            if reread_len != len || reread.len() != kept || reread.last().and_then(entry_id) != Some(leaf) {
                return None;
            }
            reread
        };
        cached.leaf_id = tail.last().and_then(entry_id)?.to_owned();
        cached.len = len;
        Some((cached, tail))
    }
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
    let tails = Arc::new(ToolBranchTailCache::default());
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
        let tails = Arc::clone(&tails);
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
            let (branch_len, tail): (usize, Arc<Vec<Value>>) = if (kind == maho_ext_api::EventKind::ToolCall || kind == maho_ext_api::EventKind::ToolResult)
                && sink.wants_branch(kind, session_id)
            {
                tails.tail(session_id, context.session_manager.as_ref())
            } else {
                if kind == maho_ext_api::EventKind::SessionShutdown {
                    tails.forget(session_id);
                }
                (entries.len(), Arc::default())
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Mutex;

    struct Branch(Mutex<Vec<Value>>);
    impl maho_ext_api::ToolSessionManager for Branch {
        fn session_id(&self) -> &str { "session" }
        fn session_file(&self) -> Option<&std::path::Path> { None }
    }
    impl maho_ext_api::SessionManager for Branch {
        fn get_entries(&self) -> Vec<maho_ext_api::SessionEntry> { self.get_branch() }
        fn get_branch(&self) -> Vec<maho_ext_api::SessionEntry> {
            self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).iter().map(|data| maho_ext_api::SessionEntry {
                id: data["id"].as_str().unwrap_or_default().to_owned(), parent_id: None, timestamp: String::new(), kind: "message".into(), data: data.clone(),
            }).collect()
        }
        fn get_leaf_id(&self) -> Option<String> { None }
        fn get_session_name(&self) -> Option<String> { None }
    }

    fn entry(index: usize) -> Value {
        let (role, text) = match index % 7 { 0 => ("user", "prompt"), 1 | 4 => ("assistant", "reply"), 2 => ("toolResult", "out"), 5 => ("assistant", " "), _ => ("toolResult", "") };
        json!({"id": format!("e{index}"), "type": "message", "message": {"role": role, "content": text}})
    }

    #[test]
    fn cached_tail_matches_full_branch_scan_while_the_branch_grows() {
        let branch = Branch(Mutex::new(Vec::new()));
        let cache = ToolBranchTailCache::default();
        let mut next = 0;
        for step in 0..120 {
            for _ in 0..(step % 9) {
                branch.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(entry(next));
                next += 1;
            }
            let (len, tail) = cache.tail("session", &branch);
            assert_eq!((len, tail.as_ref().clone()), tool_branch_tail(&branch), "step {step}");
            // The cache retains only per-entry marks for the kept tail, never the payloads.
            let sessions = cache.sessions.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(cached) = sessions.get("session") {
                assert_eq!((cached.len, cached.marks.len(), Some(cached.leaf_id.as_str())), (len, tail.len(), tail.last().and_then(entry_id)), "step {step}");
            }
        }
        // Re-reading the same branch with nothing appended still yields the identical tail.
        let (len, tail) = cache.tail("session", &branch);
        assert_eq!((len, tail.as_ref().clone()), tool_branch_tail(&branch));
        // A branch that no longer extends the cached leaf falls back to the full scan.
        branch.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).truncate(10);
        let (len, tail) = cache.tail("session", &branch);
        assert_eq!((len, tail.as_ref().clone()), tool_branch_tail(&branch));
    }
}
