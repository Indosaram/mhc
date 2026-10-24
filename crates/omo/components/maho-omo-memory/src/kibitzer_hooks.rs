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

/// What a tool hook captured from the branch: the TRUE branch length (the events cursor), the
/// newest [`RECALL_PATH_ENTRY_WINDOW`] raw entries (`entries`, the mention-exclusion window) and the
/// decisive `summary` entries in branch order. `summary` holds every entry `newest_assistant_text`,
/// `user_texts` and `judge_transcript` can return on the full branch, so each yields exactly its
/// full-branch result over `summary`.
pub struct BranchTail<'a> {
    pub len: usize,
    pub entries: &'a [Value],
    pub summary: &'a [Value],
}

/// The entry `newest_assistant_text`'s newest-first scan stops at: a non-blank assistant message,
/// or a `message` entry without a `message` field (the scan returns `None` there).
fn decides_assistant(entry: &Value) -> bool {
    entry.get("type").and_then(Value::as_str) == Some("message")
        && (entry.get("message").is_none() || newest_assistant_text(std::slice::from_ref(entry)).is_some())
}

fn push_window(window: &mut Vec<(usize, Value)>, index: usize, entry: &Value) {
    window.push((index, entry.clone()));
    if window.len() > RECALL_TEXT_WINDOW {
        window.remove(0);
    }
}

/// The decisive entries of a branch, keyed by branch position: the newest [`RECALL_TEXT_WINDOW`]
/// planner user texts, the newest [`RECALL_TEXT_WINDOW`] judge turns and the newest
/// assistant-deciding entry. Updated from appended entries only.
#[derive(Clone, Default)]
struct BranchSummary {
    users: Vec<(usize, Value)>,
    turns: Vec<(usize, Value)>,
    assistant: Option<(usize, Value)>,
}

impl BranchSummary {
    /// Folds `entries` (branch positions from `base`) in, oldest first.
    fn extend(&mut self, base: usize, entries: &[Value]) {
        for (offset, entry) in entries.iter().enumerate() {
            let one = std::slice::from_ref(entry);
            let index = base + offset;
            if !user_texts(one).is_empty() {
                push_window(&mut self.users, index, entry);
            }
            if !judge_transcript(one).is_empty() {
                push_window(&mut self.turns, index, entry);
            }
            if decides_assistant(entry) {
                self.assistant = Some((index, entry.clone()));
            }
        }
    }

    /// The decisive entries in branch order once the branch's newest entry (`newest`, at branch
    /// position `index`) is folded in. That entry is never kept in the cache: it may still change.
    fn view(&self, index: usize, newest: Option<&Value>) -> Vec<Value> {
        let mut summary = self.clone();
        summary.extend(index, newest.map(std::slice::from_ref).unwrap_or_default());
        summary.entries()
    }

    /// The union of the decisive entries, in branch order.
    fn entries(&self) -> Vec<Value> {
        let mut kept: Vec<&(usize, Value)> = self.users.iter().chain(&self.turns).chain(&self.assistant).collect();
        kept.sort_by_key(|(index, _)| *index);
        kept.dedup_by_key(|(index, _)| *index);
        kept.into_iter().map(|(_, entry)| entry.clone()).collect()
    }
}

fn entry_id(entry: &Value) -> Option<&str> {
    entry.get("id").and_then(Value::as_str)
}

/// One tool hook's capture: see [`BranchTail`].
pub struct ToolBranchView {
    pub len: usize,
    pub window: Vec<Value>,
    pub summary: Vec<Value>,
}

/// A session's cached [`BranchSummary`] of every branch entry before the leaf, plus the leaf and
/// branch length it was taken at. The leaf is re-read and re-folded by the next hook.
#[derive(Clone)]
struct CachedBranch {
    leaf_id: String,
    len: usize,
    summary: BranchSummary,
}

/// Per-session tool-hook branch bookkeeping, extended incrementally: a hook re-reads only the
/// newest [`RECALL_PATH_ENTRY_WINDOW`] entries (further back only to reach the cached leaf) and folds
/// the entries appended since that leaf into the summary. Any other branch change (leaf moved off
/// the cached chain, trimmed history) falls back to a scan back to the decisive entries.
#[derive(Default)]
pub struct ToolBranchTailCache {
    sessions: std::sync::Mutex<std::collections::BTreeMap<String, CachedBranch>>,
}

impl ToolBranchTailCache {
    pub fn tail(&self, session_id: &str, session_manager: &dyn maho_ext_api::SessionManager) -> ToolBranchView {
        // Cloned, not removed: an overlapping hook for the same session still finds the entry
        // and extends it instead of falling back to the full scan.
        let cached = self.sessions.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(session_id).cloned();
        let (next, window) = cached.and_then(|cached| Self::extend(cached, session_manager)).unwrap_or_else(|| Self::full(session_manager));
        let view = ToolBranchView { len: next.len, summary: next.summary.view(next.len.saturating_sub(1), window.last()), window };
        if !next.leaf_id.is_empty() {
            self.sessions.lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(session_id.to_owned(), next);
        }
        view
    }

    pub fn forget(&self, session_id: &str) {
        self.sessions.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(session_id);
    }

    fn full(session_manager: &dyn maho_ext_api::SessionManager) -> (CachedBranch, Vec<Value>) {
        let (mut users, mut turns, mut assistant) = (0usize, 0usize, false);
        let mut satisfied = |entry: &Value| {
            let one = std::slice::from_ref(entry);
            assistant = assistant || decides_assistant(entry);
            users += user_texts(one).len();
            turns += judge_transcript(one).len();
            assistant && users >= RECALL_TEXT_WINDOW && turns >= RECALL_TEXT_WINDOW
        };
        let (len, tail) = session_manager.get_branch_tail(RECALL_PATH_ENTRY_WINDOW, &mut satisfied);
        let mut window: Vec<Value> = tail.into_iter().map(|entry| entry.data).collect();
        let mut summary = BranchSummary::default();
        summary.extend(len - window.len(), &window[..window.len().saturating_sub(1)]);
        let leaf_id = window.last().and_then(entry_id).unwrap_or_default().to_owned();
        window.drain(..window.len().saturating_sub(RECALL_PATH_ENTRY_WINDOW));
        (CachedBranch { leaf_id, len, summary }, window)
    }

    fn extend(mut cached: CachedBranch, session_manager: &dyn maho_ext_api::SessionManager) -> Option<(CachedBranch, Vec<Value>)> {
        let leaf = std::mem::take(&mut cached.leaf_id);
        let (len, tail) = session_manager.get_branch_tail(RECALL_PATH_ENTRY_WINDOW, &mut |entry| entry_id(entry) == Some(leaf.as_str()));
        let mut window: Vec<Value> = tail.into_iter().map(|entry| entry.data).collect();
        let at = window.iter().rposition(|entry| entry_id(entry) == Some(leaf.as_str()))?;
        if len != cached.len + (window.len() - at - 1) {
            return None;
        }
        // Fold the re-read previous leaf and the appended entries, all but the new leaf.
        cached.summary.extend(cached.len - 1, &window[at..window.len() - 1]);
        cached.leaf_id = window.last().and_then(entry_id)?.to_owned();
        cached.len = len;
        window.drain(..window.len().saturating_sub(RECALL_PATH_ENTRY_WINDOW));
        Some((cached, window))
    }
}

/// The synchronous capture surface the composition implements.
///
/// `entries` is the REAL branch snapshot (`context.session_manager.get_branch()`'s `data`), captured
/// while the ctx is still alive. The consumer derives the events cursor from its LENGTH (the branch
/// length), never from a synthetic monotonic counter. The tool hooks receive a [`BranchTail`]: the
/// newest mention window and the decisive summary entries of that snapshot plus its true length. `prompt` is the `before_agent_start` event's
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
            // The tool hooks fire per tool call: capture only the mention window and decisive summary
            // every consumer reads (see `BranchTail`) plus the TRUE branch length as the cursor.
            let view = if (kind == maho_ext_api::EventKind::ToolCall || kind == maho_ext_api::EventKind::ToolResult)
                && sink.wants_branch(kind, session_id)
            {
                tails.tail(session_id, context.session_manager.as_ref())
            } else {
                if matches!(
                    kind,
                    maho_ext_api::EventKind::AgentSettled
                        | maho_ext_api::EventKind::SessionShutdown
                        | maho_ext_api::EventKind::SessionCompact
                ) {
                    tails.forget(session_id);
                }
                ToolBranchView { len: entries.len(), window: Vec::new(), summary: Vec::new() }
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
                            sink.on_tool_call(session_id, &tool_call_id, &tool_name, &input, &BranchTail { len: view.len, entries: &view.window, summary: &view.summary });
                        }
                    }
                    maho_ext_api::EventKind::ToolResult => {
                        if let (Some((tool_call_id, tool_name, input, content, is_error)), Some(gate)) = (captured, gate) {
                            sink.on_tool_result(session_id, &KibitzerToolResultEvent { tool_call_id: &tool_call_id, tool_name: &tool_name, input: &input, content: &content, is_error }, &BranchTail { len: view.len, entries: &view.window, summary: &view.summary }, &gate);
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

    /// A branch whose `get_branch_tail` records how many entries each call materialized.
    struct Branch(Mutex<Vec<Value>>, Mutex<Vec<usize>>);
    impl Branch {
        fn new() -> Self { Self(Mutex::new(Vec::new()), Mutex::new(Vec::new())) }
        fn push(&self, value: Value) { self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(value); }
        fn data(&self) -> Vec<Value> { self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone() }
        fn take_reads(&self) -> Vec<usize> { std::mem::take(&mut *self.1.lock().unwrap_or_else(std::sync::PoisonError::into_inner)) }
    }
    fn session_entry(data: &Value) -> maho_ext_api::SessionEntry {
        maho_ext_api::SessionEntry { id: data["id"].as_str().unwrap_or_default().to_owned(), parent_id: None, timestamp: String::new(), kind: "message".into(), data: data.clone() }
    }
    impl maho_ext_api::ToolSessionManager for Branch {
        fn session_id(&self) -> &str { "session" }
        fn session_file(&self) -> Option<&std::path::Path> { None }
    }
    impl maho_ext_api::SessionManager for Branch {
        fn get_entries(&self) -> Vec<maho_ext_api::SessionEntry> { self.get_branch() }
        fn get_branch(&self) -> Vec<maho_ext_api::SessionEntry> { self.data().iter().map(session_entry).collect() }
        /// The host's lazy shape: walk newest-first and materialize only the returned suffix.
        fn get_branch_tail(&self, min_entries: usize, satisfied: &mut dyn FnMut(&Value) -> bool) -> (usize, Vec<maho_ext_api::SessionEntry>) {
            let data = self.data();
            let len = data.len();
            let (mut start, mut done) = (len, false);
            while start > 0 && (len - start < min_entries || !done) {
                start -= 1;
                if !done { done = satisfied(&data[start]); }
            }
            self.1.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(len - start);
            (len, data[start..].iter().map(session_entry).collect())
        }
        fn get_leaf_id(&self) -> Option<String> { None }
        fn get_session_name(&self) -> Option<String> { None }
    }

    fn entry(index: usize) -> Value {
        let (role, text) = match index % 7 { 0 => ("user", "prompt"), 1 | 4 => ("assistant", "reply"), 2 => ("toolResult", "out"), 5 => ("assistant", " "), _ => ("toolResult", "") };
        json!({"id": format!("e{index}"), "type": "message", "message": {"role": role, "content": format!("{text}{index}")}})
    }

    /// Every consumer input equals the full-branch scan's.
    fn assert_matches_full_branch(view: &ToolBranchView, branch: &Branch, step: usize) {
        let full = branch.data();
        assert_eq!(view.len, full.len(), "step {step}");
        assert_eq!(view.window, full[full.len().saturating_sub(RECALL_PATH_ENTRY_WINDOW)..].to_vec(), "step {step}");
        assert_eq!(user_texts(&view.summary), user_texts(&full), "step {step}");
        assert_eq!(judge_transcript(&view.summary), judge_transcript(&full), "step {step}");
        assert_eq!(newest_assistant_text(&view.summary), newest_assistant_text(&full), "step {step}");
        assert!(view.summary.len() <= 2 * RECALL_TEXT_WINDOW + 1, "step {step}");
    }

    #[test]
    fn cached_tail_matches_full_branch_scan_while_the_branch_grows() {
        let branch = Branch::new();
        let cache = ToolBranchTailCache::default();
        let mut next = 0;
        for step in 0..120 {
            for _ in 0..(step % 9) {
                branch.push(entry(next));
                next += 1;
            }
            assert_matches_full_branch(&cache.tail("session", &branch), &branch, step);
        }
        // Re-reading the same branch with nothing appended still yields the identical inputs.
        assert_matches_full_branch(&cache.tail("session", &branch), &branch, 120);
        // A leaf whose content changed after a hook read it is re-read, never served stale.
        branch.push(json!({"id": "live", "type": "message", "message": {"role": "user", "content": "partial"}}));
        assert_matches_full_branch(&cache.tail("session", &branch), &branch, 120);
        if let Some(leaf) = branch.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).last_mut() {
            leaf["message"]["content"] = json!(" ");
        }
        assert_matches_full_branch(&cache.tail("session", &branch), &branch, 120);
        // A branch that no longer extends the cached leaf falls back to the full scan.
        branch.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner).truncate(10);
        assert_matches_full_branch(&cache.tail("session", &branch), &branch, 121);
    }

    #[test]
    fn long_branch_with_few_prompts_reads_only_the_window_per_hook() {
        let branch = Branch::new();
        let cache = ToolBranchTailCache::default();
        branch.push(json!({"id": "u0", "type": "message", "message": {"role": "user", "content": "first prompt"}}));
        branch.push(json!({"id": "a0", "type": "message", "message": {"role": "assistant", "content": "plan"}}));
        let mut next = 0;
        for step in 0..400 {
            let appended = 1 + step % 3;
            for _ in 0..appended {
                let value = match next % 50 {
                    17 => json!({"id": format!("t{next}"), "type": "message", "message": {"role": "assistant", "content": format!("note {next}")}}),
                    29 => json!({"id": format!("t{next}"), "type": "message"}),
                    41 if next < 200 => json!({"id": format!("t{next}"), "type": "message", "message": {"role": "user", "content": format!("follow-up {next}")}}),
                    _ if next % 2 == 0 => json!({"id": format!("t{next}"), "type": "message", "message": {"role": "assistant", "content": [{"type": "toolCall", "name": "read"}]}}),
                    _ => json!({"id": format!("t{next}"), "type": "message", "message": {"role": "toolResult", "content": format!("out {next}")}}),
                };
                branch.push(value);
                next += 1;
            }
            branch.take_reads();
            let view = cache.tail("session", &branch);
            assert_matches_full_branch(&view, &branch, step);
            // After the first hook, each hook materializes only the mention window (or the appended run).
            if step > 0 {
                assert_eq!(branch.take_reads(), vec![RECALL_PATH_ENTRY_WINDOW.min(view.len).max(appended + 1)], "step {step}");
            }
        }
        assert!(user_texts(&branch.data()).len() < RECALL_TEXT_WINDOW);
    }

    /// A branch whose next `get_branch_tail` first runs a nested `tail()` for the same session,
    /// i.e. a second hook overlapping the first while the first is reading the branch.
    struct Overlapping { branch: Branch, cache: Arc<ToolBranchTailCache>, armed: std::sync::atomic::AtomicBool, nested: Mutex<Option<ToolBranchView>> }
    impl maho_ext_api::ToolSessionManager for Overlapping {
        fn session_id(&self) -> &str { "session" }
        fn session_file(&self) -> Option<&std::path::Path> { None }
    }
    impl maho_ext_api::SessionManager for Overlapping {
        fn get_entries(&self) -> Vec<maho_ext_api::SessionEntry> { self.branch.get_branch() }
        fn get_branch(&self) -> Vec<maho_ext_api::SessionEntry> { self.branch.get_branch() }
        fn get_branch_tail(&self, min_entries: usize, satisfied: &mut dyn FnMut(&Value) -> bool) -> (usize, Vec<maho_ext_api::SessionEntry>) {
            if self.armed.swap(false, std::sync::atomic::Ordering::SeqCst) {
                let view = self.cache.tail("session", self);
                *self.nested.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(view);
            }
            self.branch.get_branch_tail(min_entries, satisfied)
        }
        fn get_leaf_id(&self) -> Option<String> { None }
        fn get_session_name(&self) -> Option<String> { None }
    }

    #[test]
    fn overlapping_tail_calls_on_one_session_never_fall_back_to_the_full_scan() {
        let session = Overlapping { branch: Branch::new(), cache: Arc::new(ToolBranchTailCache::default()), armed: false.into(), nested: Mutex::new(None) };
        session.branch.push(json!({"id": "u0", "type": "message", "message": {"role": "user", "content": "only prompt"}}));
        for next in 0..200 {
            session.branch.push(json!({"id": format!("t{next}"), "type": "message", "message": {"role": "toolResult", "content": format!("out {next}")}}));
        }
        assert_matches_full_branch(&session.cache.tail("session", &session), &session.branch, 0);
        session.branch.push(json!({"id": "a1", "type": "message", "message": {"role": "assistant", "content": "note"}}));
        session.branch.take_reads();
        session.armed.store(true, std::sync::atomic::Ordering::SeqCst);
        let outer = session.cache.tail("session", &session);
        let nested = session.nested.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take();
        let Some(nested) = nested else { panic!("the nested hook did not run") };
        assert_matches_full_branch(&nested, &session.branch, 1);
        assert_matches_full_branch(&outer, &session.branch, 2);
        // Both overlapping hooks extend the cached entry: each reads only the mention window.
        assert_eq!(session.branch.take_reads(), vec![RECALL_PATH_ENTRY_WINDOW, RECALL_PATH_ENTRY_WINDOW]);
    }
}
