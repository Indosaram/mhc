//! Resident Kibitzer wake runner (latest `kibitzer/sidecar.ts` `startChild`/`followUp` + tools).
//!
//! Async ABI: `spawn`/`steer`/`follow_up`/`dispose` are awaited INSIDE the returned future, so a
//! current-thread host reactor is never blocked. NO `std::sync::MutexGuard` is held across an
//! `.await`: the resident branch clones its child/accumulators/state into an owned tuple under the
//! lock, releases the guard, THEN awaits. Settlement is ARMED BEFORE the trigger, so a synchronous
//! terminal/event completion is observed. The runner holds the host's explicit `spawn` executor (no
//! ambient runtime assumption); a `dispose` error is reported through `warn`, never discarded.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use memory_core::recall::RecallNudge;

use crate::kibitzer_child::{KibitzerChild, KibitzerChildObservation, KibitzerChildSpawnInput, KibitzerChildSpawner};
use crate::kibitzer_contract::{
    KibitzerWakeFuture, KibitzerWakeRequest, KibitzerWakeResult, KibitzerWakeRunner, KibitzerWakeSpawn,
    KibitzerWakeStatus,
};
use crate::kibitzer_events::{KibitzerEventCaps, KibitzerEventStream, create_kibitzer_event_stream};
use crate::kibitzer_judge_outcome::{JudgeTurnClassification, classify_judge_turn};
use crate::kibitzer_prompt::{
    KibitzerEnvelopeInput, KibitzerReseedInput, KibitzerSidecarCandidate,
    render_kibitzer_reseed_prompt, render_kibitzer_seed_prompt, render_kibitzer_wake_prompt,
};
use crate::kibitzer_prompt_blocks::{KIBITZER_FIELD_CAPS, KIBITZER_SIDECAR_TOOL_NAMES};
use crate::recall_consumer::CollectedRecallCandidates;
use crate::recall_session_read::RecallRole;

struct ResidentChild {
    child: Arc<dyn KibitzerChild>,
    generation: u64,
    running: Arc<AtomicBool>,
    nudges: Arc<Mutex<Vec<RecallNudge>>>,
    tool_calls: Arc<AtomicUsize>,
    unsubscribe_nudges: Option<Box<dyn FnOnce() + Send>>,
    unsubscribe_tools: Option<Box<dyn FnOnce() + Send>>,
}

/// The production runner. `spawner` and `spawn` are required; there is no no-op default.
pub struct ResidentKibitzerRunner {
    spawner: Arc<dyn KibitzerChildSpawner>,
    spawn: KibitzerWakeSpawn,
    caps: KibitzerEventCaps,
    task_summary: Option<String>,
    warn: Arc<dyn Fn(&str) + Send + Sync>,
    sessions: Arc<Mutex<BTreeMap<String, ResidentChild>>>,
    generations: Arc<Mutex<BTreeMap<String, u64>>>,
}

impl ResidentKibitzerRunner {
    pub fn new(
        spawner: Arc<dyn KibitzerChildSpawner>,
        spawn: KibitzerWakeSpawn,
        caps: KibitzerEventCaps,
        task_summary: Option<String>,
        warn: Arc<dyn Fn(&str) + Send + Sync>,
    ) -> Self {
        Self { spawner, spawn, caps, task_summary, warn, sessions: Arc::new(Mutex::new(BTreeMap::new())), generations: Arc::new(Mutex::new(BTreeMap::new())) }
    }

    /// The current generation for `session_id` (0 when no child was ever created).
    pub fn generation(&self, session_id: &str) -> u64 {
        self.generations.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(session_id).copied().unwrap_or(0)
    }

    fn drop_child(&self, session_id: &str) -> Option<ResidentChild> {
        self.sessions.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(session_id)
    }

    /// Unsubscribe both listeners then schedule the async `dispose` on the host executor; a dispose
    /// failure is reported through `warn`, never silently dropped.
    fn teardown(spawn: &KibitzerWakeSpawn, warn: &Arc<dyn Fn(&str) + Send + Sync>, resident: &mut ResidentChild) {
        if let Some(unsubscribe) = resident.unsubscribe_nudges.take() { unsubscribe(); }
        if let Some(unsubscribe) = resident.unsubscribe_tools.take() { unsubscribe(); }
        let child = Arc::clone(&resident.child);
        let warn = Arc::clone(warn);
        spawn(Box::pin(async move {
            if let Err(error) = child.dispose().await {
                warn(&format!("kibitzer child dispose failed: {error}"));
            }
        }));
    }
}

impl KibitzerWakeRunner for ResidentKibitzerRunner {
    fn start(&self, request: KibitzerWakeRequest) -> KibitzerWakeFuture {
        let spawner = Arc::clone(&self.spawner);
        let spawn = Arc::clone(&self.spawn);
        let sessions = Arc::clone(&self.sessions);
        let generations = Arc::clone(&self.generations);
        let warn = Arc::clone(&self.warn);
        let caps = self.caps;
        let task_summary = self.task_summary.clone();
        let session_id = request.session_id.clone();
        let candidates = request.candidates.clone();
        let cancel = Arc::clone(&request.cancel);
        let max_tool_budget = request.max_tool_budget;
        Box::pin(async move {
            let prepared = match prepare(&spawner, caps, task_summary.as_deref(), &sessions, &generations, &session_id, &candidates, max_tool_budget).await {
                Ok(prepared) => prepared,
                Err(_) => return KibitzerWakeResult { nudges: Vec::new(), status: KibitzerWakeStatus::Failed },
            };
            let child = Arc::clone(&prepared.child);
            // ARM settlement BEFORE the trigger so a synchronous terminal is observed.
            let settle = child.settle();
            let trigger = match &prepared.trigger {
                Trigger::Steer(prompt) => child.steer(prompt).await,
                Trigger::FollowUp(prompt) => child.follow_up(prompt).await,
            };
            if let Err(error) = trigger {
                // A first-trigger failure after subscription must not leak the resident or child.
                if prepared.fresh
                    && let Some(mut resident) = sessions.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(&prepared.session_id)
                {
                    Self::teardown(&spawn, &warn, &mut resident);
                }
                warn(&format!("kibitzer wake trigger failed: {error}"));
                return KibitzerWakeResult { nudges: Vec::new(), status: KibitzerWakeStatus::Failed };
            }
            prepared.running.store(true, Ordering::SeqCst);
            let settle_outcome = settle.await;
            let nudges = prepared.nudges.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
            let tool_calls = prepared.tool_calls.load(Ordering::SeqCst);
            prepared.running.store(false, Ordering::SeqCst);
            if tool_calls > max_tool_budget || (cancel)() {
                child.abort();
                return KibitzerWakeResult { nudges, status: KibitzerWakeStatus::Cancelled };
            }
            let status = match classify_judge_turn(&settle_outcome, &nudges) {
                JudgeTurnClassification::Completed => KibitzerWakeStatus::Completed,
                JudgeTurnClassification::Empty => KibitzerWakeStatus::Empty,
                JudgeTurnClassification::Failed { .. } => KibitzerWakeStatus::Failed,
                JudgeTurnClassification::Dropped { .. } => KibitzerWakeStatus::Dropped,
            };
            KibitzerWakeResult { nudges, status }
        })
    }

    fn abort(&self, session_id: &str) {
        if let Some(resident) = self.sessions.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(session_id) {
            resident.child.abort();
        }
    }

    fn request_reseed(&self, session_id: &str) {
        if let Some(mut resident) = self.drop_child(session_id) {
            Self::teardown(&self.spawn, &self.warn, &mut resident);
        }
    }

    fn dispose(&self, session_id: &str) {
        if let Some(mut resident) = self.drop_child(session_id) {
            Self::teardown(&self.spawn, &self.warn, &mut resident);
        }
    }
}

enum Trigger { Steer(String), FollowUp(String) }

struct PreparedWake {
    child: Arc<dyn KibitzerChild>,
    nudges: Arc<Mutex<Vec<RecallNudge>>>,
    tool_calls: Arc<AtomicUsize>,
    running: Arc<AtomicBool>,
    trigger: Trigger,
    fresh: bool,
    session_id: String,
}

/// The async prepare. NO map guard is held across an `.await`: the resident branch clones its
/// handles into an owned tuple and releases the guard before returning.
async fn prepare(
    spawner: &Arc<dyn KibitzerChildSpawner>,
    caps: KibitzerEventCaps,
    task_summary: Option<&str>,
    sessions: &Arc<Mutex<BTreeMap<String, ResidentChild>>>,
    generations: &Arc<Mutex<BTreeMap<String, u64>>>,
    session_id: &str,
    candidates: &CollectedRecallCandidates,
    max_tool_budget: usize,
) -> Result<PreparedWake, String> {
    // --- resident branch: clone into an owned tuple, drop the guard, then decide. ---
    let resident = {
        let sessions = sessions.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        sessions.get(session_id).map(|resident| (
            Arc::clone(&resident.child),
            resident.generation,
            resident.running.load(Ordering::SeqCst),
            Arc::clone(&resident.nudges),
            Arc::clone(&resident.tool_calls),
            Arc::clone(&resident.running),
        ))
    };
    if let Some((child, _generation, running, nudges, tool_calls, running_flag)) = resident {
        let events = event_batch(caps, candidates, false);
        let sidecar_candidates = sidecar_candidates(candidates);
        let cursor = candidates.transcript.len();
        let prompt = render_kibitzer_wake_prompt(&KibitzerEnvelopeInput {
            session_id,
            max_items: candidates.max_items,
            events: &events,
            candidates: &sidecar_candidates,
            digest: None,
            task_summary: None,
            tool_budget: Some(max_tool_budget),
            caps: KIBITZER_FIELD_CAPS,
            event_window: None,
            cursor_span: Some((cursor, cursor)),
        });
        let trigger = if running {
            // A steer joins the running turn: KEEP its accumulator and tool budget.
            Trigger::Steer(prompt)
        } else {
            // A fresh turn: reset the turn-local accumulator and budget (under the lock, no await).
            nudges.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clear();
            tool_calls.store(0, Ordering::SeqCst);
            Trigger::FollowUp(prompt)
        };
        return Ok(PreparedWake { child, nudges, tool_calls, running: running_flag, trigger, fresh: false, session_id: session_id.to_string() });
    }

    // --- new child branch: monotonic generation, spawn, subscribe, REGISTER, no trigger here. ---
    let generation = {
        let mut generations = generations.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let entry = generations.entry(session_id.to_string()).or_insert(0);
        *entry += 1;
        *entry
    };
    let nudges: Arc<Mutex<Vec<RecallNudge>>> = Arc::new(Mutex::new(Vec::new()));
    let tool_calls = Arc::new(AtomicUsize::new(0));
    let running = Arc::new(AtomicBool::new(true));
    let input = KibitzerChildSpawnInput { session_id: session_id.to_string(), generation, tools: tools_of(), max_items: candidates.max_items };
    let child = spawner.spawn(input).await.map_err(|error| error.message)?;
    // Listeners registered BEFORE the trigger; the tool listener aborts THIS child on budget.
    let collected = Arc::clone(&nudges);
    let nudge_listener: Arc<dyn Fn(RecallNudge) + Send + Sync> = Arc::new(move |nudge| {
        collected.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(nudge);
    });
    let counted = Arc::clone(&tool_calls);
    let abort_child = Arc::clone(&child);
    let tool_listener: Arc<dyn Fn(KibitzerChildObservation) + Send + Sync> = Arc::new(move |event| {
        // The wake budget charges tool STARTS only (upstream `sidecar-turn.ts` `turn.toolStarts`); a
        // `ToolEnd` or `MessageEnd` observation is not a charge and never aborts.
        if matches!(event, KibitzerChildObservation::ToolStart { .. }) {
            let count = counted.fetch_add(1, Ordering::SeqCst) + 1;
            if count > max_tool_budget { abort_child.abort(); }
        }
    });
    let unsubscribe_nudges = child.subscribe_nudges(nudge_listener);
    let unsubscribe_tools = child.subscribe_observations(tool_listener);
    // Register the resident BEFORE the trigger so `abort(session_id)` finds it while pending.
    sessions.lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(session_id.to_string(), ResidentChild {
        child: Arc::clone(&child), generation, running: Arc::clone(&running),
        nudges: Arc::clone(&nudges), tool_calls: Arc::clone(&tool_calls),
        unsubscribe_nudges: Some(unsubscribe_nudges), unsubscribe_tools: Some(unsubscribe_tools),
    });
    let events = event_batch(caps, candidates, true);
    let sidecar_candidates = sidecar_candidates(candidates);
    let cursor = candidates.transcript.len();
    let prompt = if generation == 1 {
        render_kibitzer_seed_prompt(&KibitzerEnvelopeInput {
            session_id,
            max_items: candidates.max_items,
            events: &events,
            candidates: &sidecar_candidates,
            digest: None,
            task_summary,
            tool_budget: Some(max_tool_budget),
            caps: KIBITZER_FIELD_CAPS,
            event_window: None,
            cursor_span: Some((cursor, cursor)),
        })
    } else {
        render_kibitzer_reseed_prompt(&KibitzerReseedInput {
            session_id,
            max_items: candidates.max_items,
            last_cursor: cursor,
            task_summary: task_summary.unwrap_or(""),
            rejected_paths: &[],
            delivered_paths: &[],
            tool_budget: Some(max_tool_budget),
            caps: KIBITZER_FIELD_CAPS,
            max_chars: None,
        })
    };
    Ok(PreparedWake { child, nudges, tool_calls, running, trigger: Trigger::FollowUp(prompt), fresh: true, session_id: session_id.to_string() })
}

fn event_batch(caps: KibitzerEventCaps, candidates: &CollectedRecallCandidates, with_assistant: bool) -> KibitzerEventStream {
    let mut events = create_kibitzer_event_stream(caps);
    let cursor = candidates.transcript.len();
    for turn in &candidates.transcript {
        match turn.role {
            RecallRole::User => { events.on_prompt(&turn.text, cursor); }
            RecallRole::Assistant => {
                if with_assistant {
                    events.newest_assistant(&[serde_json::json!({ "type": "message", "message": { "role": "assistant", "content": turn.text } })], cursor);
                }
            }
        }
    }
    events
}

fn tools_of() -> Vec<String> { KIBITZER_SIDECAR_TOOL_NAMES.iter().map(|name| (*name).to_string()).collect() }

/// The envelope's candidate view over the collected candidates. The runner owns this mapping, so a
/// candidate field can never be silently dropped between collection and the child's envelope.
fn sidecar_candidates(candidates: &CollectedRecallCandidates) -> Vec<KibitzerSidecarCandidate> {
    candidates
        .candidates
        .iter()
        .map(|candidate| KibitzerSidecarCandidate {
            path: candidate.path.clone(),
            description: Some(candidate.description.clone()),
            excerpt: Some(candidate.excerpt.clone()),
            score: Some(candidate.score),
        })
        .collect()
}

#[cfg(test)]
#[path = "kibitzer_runner_tests.rs"]
mod tests;
