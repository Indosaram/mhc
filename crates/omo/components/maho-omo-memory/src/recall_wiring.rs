//! Kibitzer recall wiring - the resident composition root (latest `recall-wiring.ts` + `index.ts`).
//!
//! Owns ONE resident sidecar per bound session, binds the `KibitzerHookSink` to sidecar + delivery,
//! registers the prompt drain (renderers + `before_agent_start` injection) and the seven kibitzer
//! hooks, and binds `session_shutdown`/`session_compact` to real handlers. There is NO no-op
//! fallback: the wake runner is injected, so live recall either runs a real child or reports buffered.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use memory_core::git::GitMemoryRepo;
use memory_core::recall::{RecallCorpusCache, RecallCorpusCacheOptions, RecallLedger};
use serde_json::Value;

use crate::context::MemoryIdentityContext;
use crate::kibitzer_child::KibitzerChildSpawner;
use crate::kibitzer_contract::KibitzerSidecarTimers;
use crate::kibitzer_delivery::{
    AppendEntry, KibitzerDelivery, KibitzerDeliveryOptions, KibitzerIdleCoordinator,
    KibitzerPendingFor, KibitzerSteerMessage, KibitzerToolResultGate,
};
use crate::kibitzer_events::KibitzerEventCaps;
use crate::kibitzer_hooks::{KibitzerHookSink, KibitzerToolResultEvent, default_gate_resolver, register_kibitzer_hooks};
use crate::kibitzer_session_resources::KibitzerSessionResources;
use crate::kibitzer_sidecar::{KibitzerOfferInput, KibitzerSidecar, KibitzerSidecarOptions, KibitzerWakeSpawn};
use crate::kibitzer_sidecar_admission::KibitzerBlockingExecutor;
use crate::kibitzer_sidecar_wake::KibitzerWakeDeliver;
use crate::kibitzer_wake_slot::{KibitzerWakeSlot, KibitzerWakeSlotOptions};
use crate::recall_consumer::{CollectedRecallCandidates, CollectRecallCandidatesInput, collect_recall_candidates};
use crate::recall_drain::{DrainPendingFor, DrainQueued, EnvLookup, RecallDrainOptions, create_recall_drain};
use crate::recall_planner_tools::{ToolArgWindow, tool_arg_texts};
use crate::recall_session_read::{RecallSessionSnapshot, snapshot_session};
use crate::recall_transcript_mentions::{BranchMentionIndex, MentionDocument, TranscriptMentionIndex, TranscriptMentionInput, create_transcript_mention_index};
use crate::worker::completion_renderers::ResolveEntryTheme;

const CHILD_SENTINELS: [&str; 2] = ["SENPI_MEMORY_REFLECTION", "SENPI_MEMORY_FACTS"];

/// Open the identity's git memory repo (`options.createRepo`).
pub type CreateRepo = Arc<dyn Fn(&MemoryIdentityContext) -> Result<GitMemoryRepo, String> + Send + Sync>;

pub struct MemoryRecallWiringOptions {
    pub resolve_context: crate::prompt::PromptContextResolver,
    /// The resolved settings, fail-open: an `Err` falls back to defaults, never disables recall.
    pub resolve_settings: Arc<dyn Fn() -> Result<Value, String> + Send + Sync>,
    pub env: EnvLookup,
    pub create_repo: CreateRepo,
    pub ledger_for: Arc<dyn Fn(&MemoryIdentityContext) -> RecallLedger + Send + Sync>,
    pub pending_for: KibitzerPendingFor,
    pub coordinator: Option<Arc<dyn KibitzerIdleCoordinator>>,
    pub send_message: Arc<dyn Fn(KibitzerSteerMessage) -> Result<(), String> + Send + Sync>,
    pub append_entry: AppendEntry,
    pub spawn: KibitzerWakeSpawn,
    pub timers: Arc<dyn KibitzerSidecarTimers>,
    pub now_ms: Arc<dyn Fn() -> i64 + Send + Sync>,
    pub random: Arc<dyn Fn() -> f64 + Send + Sync>,
    pub warn: Arc<dyn Fn(&str) + Send + Sync>,
    /// The real child spawner the resident sidecar builds its idle child with. Required; no no-op default.
    pub spawner: Arc<dyn KibitzerChildSpawner>,
    /// The retained native blocking executor the machine-wide wake lease acquire runs on.
    pub executor: Arc<dyn KibitzerBlockingExecutor>,
    /// `memory.recall.event_caps`: the event stream's field caps, resolved by the composition.
    /// `None` keeps the stream defaults (`create_kibitzer_event_stream(..unwrap_or_default())`).
    pub event_caps: Option<KibitzerEventCaps>,
    /// The ONE per-session resources registry getter: the sidecar and the member tools share the
    /// identical `KibitzerSessionResources` for a session, never a copy.
    pub session_resources_for: Arc<dyn Fn(&str) -> KibitzerSessionResources + Send + Sync>,
    pub tool_budget: Option<usize>,
    pub max_concurrent_wakes: Option<usize>,
    pub sidecar_max_tokens: Option<i64>,
    /// The prompt drain options (renderers + `before_agent_start` injection). `resolve_settings` and
    /// `pending_for` here are the ACTUAL `recall_drain` option types.
    pub drain_resolve_settings: Arc<dyn Fn() -> Value + Send + Sync>,
    pub drain_pending_for: DrainPendingFor,
    pub drain_queued: Option<DrainQueued>,
}

pub struct MemoryRecallWiring {
    options: MemoryRecallWiringOptions,
    delivery: Arc<KibitzerDelivery>,
    sidecars: Mutex<BTreeMap<String, Arc<KibitzerSidecar>>>,
    corpus_cache: Mutex<RecallCorpusCache>,
    mentions: Mutex<BranchMentionIndex>,
    tool_args: Mutex<ToolArgWindow>,
}

impl MemoryRecallWiring {
    pub fn new(options: MemoryRecallWiringOptions) -> Arc<Self> {
        let delivery = KibitzerDelivery::new(KibitzerDeliveryOptions {
            ledger_for: options.ledger_for.clone(),
            pending_for: options.pending_for.clone(),
            coordinator: options.coordinator.clone(),
            send_message: options.send_message.clone(),
            append_entry: options.append_entry.clone(),
            warn: options.warn.clone(),
        });
        Arc::new(Self {
            options,
            delivery,
            sidecars: Mutex::new(BTreeMap::new()),
            corpus_cache: Mutex::new(RecallCorpusCache::new(RecallCorpusCacheOptions::default())),
            mentions: Mutex::new(create_transcript_mention_index()),
            tool_args: Mutex::new(ToolArgWindow::new()),
        })
    }

    pub fn delivery(&self) -> Arc<KibitzerDelivery> {
        Arc::clone(&self.delivery)
    }

    /// `register`: the prompt drain (renderers + injection) plus the seven kibitzer hooks bound to
    /// the resident sidecar/delivery. Uses the ACTUAL `recall_drain` option types.
    pub fn register(self: &Arc<Self>, api: &mut maho_ext_api::ExtensionApi, theme: ResolveEntryTheme) {
        let drain = create_recall_drain(RecallDrainOptions {
            resolve_context: self.options.resolve_context.clone(),
            resolve_settings: Arc::clone(&self.options.drain_resolve_settings),
            env: self.options.env.clone(),
            ledger_for: self.options.ledger_for.clone(),
            pending_for: Arc::clone(&self.options.drain_pending_for),
            drain_queued: self.options.drain_queued.clone(),
            warn: self.options.warn.clone(),
        });
        drain.register(api, theme);
        let sink: Arc<dyn KibitzerHookSink> = Arc::clone(self) as Arc<dyn KibitzerHookSink>;
        register_kibitzer_hooks(api, sink, default_gate_resolver());
    }

    /// Fail-open recall settings: a resolver `Err` becomes `Null` (defaults); recall is disabled
    /// only by an explicit `enabled: false`. Resolved to a concrete `Value` ONCE.
    fn recall_settings(&self, identity: &str) -> Value {
        let settings = match (self.options.resolve_settings)() {
            Ok(value) => value,
            Err(error) => {
                (self.options.warn)(&format!("memory recall settings read failed; using defaults: {error}"));
                Value::Null
            }
        };
        crate::reflection_settings::resolve_agent_recall_settings(Some(&settings), identity).unwrap_or(Value::Null)
    }

    fn child_sentinel(&self) -> bool {
        CHILD_SENTINELS.iter().any(|name| (self.options.env)(name).as_deref() == Some("1"))
    }

    /// `collectCandidatesFromSnapshot`: the ctx-free collection over a captured snapshot.
    pub fn collect_candidates_from_snapshot(&self, snapshot: &RecallSessionSnapshot, extra_texts: &[String]) -> Option<CollectedRecallCandidates> {
        if self.child_sentinel() {
            return None;
        }
        let context = (self.options.resolve_context)(&snapshot.id)?;
        let recall_settings = self.recall_settings(&context.identity);
        if recall_settings.get("enabled").and_then(Value::as_bool) == Some(false) {
            return None;
        }
        let repo = match (self.options.create_repo)(&context) {
            Ok(repo) => repo,
            Err(error) => {
                (self.options.warn)(&format!("memory recall repo open failed; skipping this batch: {error}"));
                return None;
            }
        };
        let corpus = self.corpus_cache.lock().unwrap_or_else(std::sync::PoisonError::into_inner).load(&repo).ok()?;
        if corpus.documents.is_empty() {
            return None;
        }
        let documents: Vec<MentionDocument> = corpus.documents.iter().map(|document| MentionDocument { path: document.path.clone() }).collect();
        let exclude_paths = self.mentions.lock().unwrap_or_else(std::sync::PoisonError::into_inner).excluded_paths(TranscriptMentionInput {
            session_id: &snapshot.id,
            entries: &snapshot.entries,
            documents: &documents,
        });
        let ledger = (self.options.ledger_for)(&context);
        collect_recall_candidates(CollectRecallCandidatesInput {
            documents: &corpus.documents,
            snapshot,
            identity: &context.identity,
            recall_settings: &recall_settings,
            ledger: &ledger,
            extra_texts,
            exclude_paths: &exclude_paths,
        })
        .ok()
        .flatten()
    }

    /// `collectCandidates`: build the snapshot from a branch and delegate to the ctx-free path.
    pub fn collect_candidates(&self, session_id: &str, entries: &[Value], extra_texts: &[String]) -> Option<CollectedRecallCandidates> {
        let snapshot = snapshot_session(session_id, entries)?;
        self.collect_candidates_from_snapshot(&snapshot, extra_texts)
    }

    /// The lazily-created resident sidecar for a session.
    fn sidecar_for(&self, session_id: &str, context: &MemoryIdentityContext) -> Arc<KibitzerSidecar> {
        let mut sidecars = self.sidecars.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(sidecar) = sidecars.get(session_id) {
            return Arc::clone(sidecar);
        }
        let now = Arc::clone(&self.options.now_ms);
        let wake_slot = Arc::new(KibitzerWakeSlot::new(KibitzerWakeSlotOptions {
            locks_directory: context.identity_paths.locks.clone(),
            max_concurrent: self.options.max_concurrent_wakes.unwrap_or(2),
            wait_timeout_ms: None,
            poll_ms: None,
            now: Arc::clone(&now),
        }).expect("max_concurrent_wakes is validated positive by the config resolver"));
        let sidecar = KibitzerSidecar::new(KibitzerSidecarOptions {
            session_id: session_id.to_string(),
            resources: (self.options.session_resources_for)(session_id),
            wake_slot,
            spawner: Arc::clone(&self.options.spawner),
            executor: Arc::clone(&self.options.executor),
            spawn: Arc::clone(&self.options.spawn),
            deliver: Arc::new(WiringWakeDeliver {
                delivery: Arc::clone(&self.delivery),
                session_id: session_id.to_string(),
                context: context.clone(),
            }),
            tool_budget: self.options.tool_budget,
            wake_deadline_ms: None,
            sidecar_max_tokens: self.options.sidecar_max_tokens,
            event_caps: self.options.event_caps,
            timers: Some(Arc::clone(&self.options.timers)),
            now: Some(Arc::clone(&self.options.now_ms)),
            random: Some(Arc::clone(&self.options.random)),
            on_wake: None,
            warn: Some(Arc::clone(&self.options.warn)),
        });
        sidecars.insert(session_id.to_string(), Arc::clone(&sidecar));
        sidecar
    }

    /// The EXISTING resident sidecar for a session, if any. NEVER creates one: a tool hook that fires
    /// after shutdown (or before the first wake) must not resurrect a terminal sidecar.
    fn existing_sidecar(&self, session_id: &str) -> Option<Arc<KibitzerSidecar>> {
        self.sidecars.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(session_id).map(Arc::clone)
    }
}

/// The per-sidecar delivery seam. `KibitzerWakeDeliver` carries no session/context, while
/// `KibitzerDelivery::accept` needs both, so ONE adapter is built per sidecar, capturing this
/// wiring's shared delivery policy plus the session's identity context. No global seam, no no-op.
struct WiringWakeDeliver {
    delivery: Arc<KibitzerDelivery>,
    session_id: String,
    context: MemoryIdentityContext,
}

impl KibitzerWakeDeliver for WiringWakeDeliver {
    fn deliver(&self, nudges: Vec<memory_core::recall::RecallNudge>, _wake: u64, _generation: u64) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send>> {
        let delivery = Arc::clone(&self.delivery);
        let session_id = self.session_id.clone();
        let context = self.context.clone();
        Box::pin(async move {
            delivery.accept(&session_id, &context, &nudges);
            Ok(())
        })
    }
}

impl KibitzerHookSink for MemoryRecallWiring {
    fn on_before_agent_start(&self, session_id: &str, prompt: &str, entries: &[Value]) {
        self.delivery.mark_running(session_id);
        let Some(context) = (self.options.resolve_context)(session_id) else { return; };
        let sidecar = self.sidecar_for(session_id, &context);
        // The branch snapshot is captured BEFORE the prompt that follows it; the cursor is the branch
        // length, so the events stream shares ONE cursor basis with the candidate collector.
        let cursor = entries.len();
        let _ = sidecar.on_branch(entries, cursor);
        let _ = sidecar.on_prompt(prompt, cursor);
        let extra = vec![prompt.to_owned()];
        let Some(candidates) = self.collect_candidates(session_id, entries, &extra) else { return; };
        // `task_summary` stays None: `on_prompt` already remembered the task line from the prompt.
        let input = KibitzerOfferInput {
            candidates: candidates.candidates,
            surfaced: candidates.surfaced,
            max_items: candidates.max_items,
            task_summary: None,
        };
        // The offer runs on the retained host executor, never inline: the handler must return before
        // the host disposes the context, and the wake does real child I/O.
        let warn = Arc::clone(&self.options.warn);
        (self.options.spawn)(Box::pin(async move {
            if let Err(error) = sidecar.offer(input).await {
                warn(&format!("omo-senpi kibitzer offer failed: {error}"));
            }
        }));
    }

    fn on_tool_call(&self, session_id: &str, tool_call_id: &str, tool_name: &str, input: &Value, entries: &[Value]) {
        // Capture into the EXISTING sidecar only, and capture the branch BEFORE the tool event that
        // follows it; a tool hook after shutdown must never resurrect a terminal sidecar.
        let sidecar = self.existing_sidecar(session_id);
        if let Some(sidecar) = &sidecar {
            let cursor = entries.len();
            let _ = sidecar.on_branch(entries, cursor);
            let _ = sidecar.on_tool_call(tool_name, input, cursor);
        }
        // The exact tool_call_id keys the in-flight set so simultaneous same-name calls never collapse.
        self.delivery.mark_tool_started(session_id, tool_call_id);
        let texts = tool_arg_texts(tool_name, input);
        self.tool_args.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(session_id, texts);
        let Some(sidecar) = sidecar else { return; };
        let extra = self.tool_args.lock().unwrap_or_else(std::sync::PoisonError::into_inner).texts(session_id);
        let Some(candidates) = self.collect_candidates(session_id, entries, &extra) else { return; };
        let offer = KibitzerOfferInput {
            candidates: candidates.candidates,
            surfaced: candidates.surfaced,
            max_items: candidates.max_items,
            task_summary: None,
        };
        let warn = Arc::clone(&self.options.warn);
        (self.options.spawn)(Box::pin(async move {
            if let Err(error) = sidecar.offer(offer).await {
                warn(&format!("omo-senpi kibitzer offer failed: {error}"));
            }
        }));
    }

    fn on_tool_result(&self, session_id: &str, event: &KibitzerToolResultEvent<'_>, entries: &[Value], gate: &KibitzerToolResultGate) {
        // Capture the result head into the EXISTING sidecar only, before delivery may steer a held
        // nudge. The branch is captured first, then the result content and its error flag.
        if let Some(sidecar) = self.existing_sidecar(session_id) {
            let cursor = entries.len();
            let _ = sidecar.on_branch(entries, cursor);
            // The real content is serialized through the shared serde contract (`ToolContent` is
            // `Serialize`); an impossible failure is reported, never replaced by a fabricated Null.
            match serde_json::to_value(event.content) {
                Ok(result) => {
                    let _ = sidecar.on_tool_result(event.tool_name, &result, event.is_error, cursor);
                }
                Err(error) => (self.options.warn)(&format!("omo-senpi kibitzer tool result content serialization failed: {error}")),
            }
        }
        self.delivery.mark_tool_finished(session_id, event.tool_call_id);
        if let Some(context) = (self.options.resolve_context)(session_id) {
            self.delivery.on_tool_result(session_id, &context, gate);
        }
    }

    fn on_turn_end(&self, session_id: &str) {
        self.delivery.mark_turn_ended(session_id);
    }

    fn on_agent_settled(&self, session_id: &str) {
        self.delivery.mark_settled(session_id);
    }

    /// Bound to the registered `session_shutdown` hook: mark the EXISTING sidecar closing
    /// SYNCHRONOUSLY, then dispose it on the retained executor and retract delivery AFTER it.
    fn on_session_shutdown(&self, session_id: &str) {
        // Obtain the EXISTING sidecar (never create one) under a short lock, then release the lock.
        let sidecar = self.sidecars.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(session_id).map(Arc::clone);
        // `argWindow.clear(sessionId)`: drop the session's tool-argument window synchronously, before
        // the async cleanup, matching upstream's order.
        self.tool_args.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clear(session_id);
        let Some(sidecar) = sidecar else {
            // No resident sidecar: retract delivery immediately (upstream's unconditional tail).
            self.delivery.on_session_shutdown(session_id);
            return;
        };
        // The SYNCHRONOUS half FIRST: set `closing` and abort a parked admission NOW, before the map
        // removal is visible and before the async cleanup is scheduled, so a late offer cannot start a
        // turn. NO identity resolve guard: cleanup must still run after the runtime pruned contexts.
        sidecar.request_shutdown();
        // Take it out of the map; the lock is released before the async future is scheduled.
        self.sidecars.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(session_id);
        // The async cleanup runs on the retained host executor and OWNS the delivery retraction: it
        // awaits `shutdown()` THEN retracts delivery, matching upstream's `await shutdown` before
        // `delivery.onSessionShutdown`. Idempotent: a second shutdown is a no-op.
        let delivery = Arc::clone(&self.delivery);
        let session = session_id.to_string();
        (self.options.spawn)(Box::pin(async move {
            sidecar.shutdown().await;
            delivery.on_session_shutdown(&session);
        }));
    }

    /// Bound to the registered `session_compact` hook: retract held/coordinator/pending.
    fn on_compaction_accepted(&self, session_id: &str) {
        if let Some(context) = (self.options.resolve_context)(session_id) {
            self.delivery.on_compaction_accepted(session_id, &context);
        }
    }
}

#[cfg(test)]
#[path = "recall_wiring_tests.rs"]
mod tests;
