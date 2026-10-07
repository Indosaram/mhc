//! The resident Kibitzer composition root (latest `kibitzer/sidecar.ts`).
//!
//! ONE in-process child per bound main session, created lazily on the first wake-eligible batch and
//! disposed on session shutdown, never recreated after it. The whole lifecycle is a small state
//! machine behind the per-session chain (`SidecarCore::serialized`):
//!
//! ```text
//!   idle ──wake──▶ turn_running ──settle──▶ idle
//!                       │   ▲                  │ context budget reached
//!                       │   └─ late steers replay through ONE followUp
//!                       │                      ▼
//!              child failed / no child      reseeding ──wake──▶ turn_running
//!                       ▼
//!                    backoff ──timer──▶ idle
//!   any ──shutdown──▶ disposed
//! ```
//!
//! Hook events are captured synchronously into the bounded event stream and only ever BUFFER; a
//! provider turn happens when `offer` brings a candidate path the child has not seen and the session
//! has not surfaced. `offer` runs entirely inside `core.serialized` and routes to the wake
//! transitions (`seed` / `follow_up` / `steer`). `request_shutdown` sets `closing` and aborts a parked
//! admission SYNCHRONOUSLY; `shutdown` then queues the cleanup on the chain, settles the live turn
//! inline and disposes terminally.
//!
//! This module is the public surface; the machinery is the seven sibling modules. It introduces no
//! second runner, session registry or resource registry: the ONE [`KibitzerSessionResources`] handle
//! and the ONE [`SidecarCore`] are shared with the member tools and the CLI, and the wake transitions
//! own the child spawner, the machine-wide wake slot and the delivery seam.

use std::collections::BTreeSet;
use std::sync::{Arc, PoisonError};

use memory_core::recall::RecallCandidate;
use serde_json::Value;

use crate::kibitzer_child::{JudgeSettle, KibitzerChildSpawner};
use crate::kibitzer_contract::{
    KibitzerBufferedReason, KibitzerOfferResult, KibitzerSidecarState, KibitzerSidecarTimers,
};
use crate::kibitzer_events::KibitzerEventCaps;
use crate::kibitzer_session_resources::KibitzerSessionResources;
use crate::kibitzer_sidecar_admission::{KibitzerBlockingExecutor, create_admission};
use crate::kibitzer_sidecar_core::{KibitzerSidecarCoreOptions, SidecarCore, create_sidecar_core};
use crate::kibitzer_sidecar_outcome::{KibitzerWakeAbort, KibitzerWakeOutcome};
use crate::kibitzer_sidecar_recovery::{KibitzerRecovery, create_recovery};
use crate::kibitzer_sidecar_turn::{TurnLifecycle, create_turn_lifecycle};
use crate::kibitzer_sidecar_wake::{KibitzerWakeDeliver, WakeTransitions, create_wake_transitions};
use crate::kibitzer_wake_policy::{WakeDecision, WakeSilenceReason, decide_wake};
use crate::kibitzer_wake_slot::KibitzerWakeSlot;

/// Re-exported so the consumer reads the host executor seam from this module (upstream `sidecar.ts`
/// re-exports the contract surface).
pub use crate::kibitzer_contract::KibitzerWakeSpawn;

/// `TASK_SUMMARY_HEAD_CHARS`: the task line is a one-line hint, never a transcript.
const TASK_SUMMARY_HEAD_CHARS: usize = 200;

/// The injectable configuration one resident sidecar is built from (upstream `KibitzerSidecarOptions`).
///
/// The applied `KibitzerSidecarCoreOptions` carries only the state-record half; the composition owns
/// the rest and hands each to its module: the machine-wide wake slot and the blocking executor to the
/// admission, the child spawner / host executor / delivery seam to the wake transitions, the report
/// callback to the turn lifecycle.
pub struct KibitzerSidecarOptions {
    pub session_id: String,
    /// The ONE registry-provided live-resource handle, shared with the member tools and the CLI.
    pub resources: KibitzerSessionResources,
    /// The machine-wide wake lease domain for this identity.
    pub wake_slot: Arc<KibitzerWakeSlot>,
    /// The real child spawner (creates the idle resident child).
    pub spawner: Arc<dyn KibitzerChildSpawner>,
    /// The retained native blocking executor the wake slot's synchronous acquire runs on.
    pub executor: Arc<dyn KibitzerBlockingExecutor>,
    /// The retained host executor every fire-and-forget transition is scheduled on.
    pub spawn: KibitzerWakeSpawn,
    /// The delivery seam `settle` awaits (composition-owned over `KibitzerDelivery`).
    pub deliver: Arc<dyn KibitzerWakeDeliver>,
    /// `memory.recall.tool_budget`; the pinned default when absent.
    pub tool_budget: Option<usize>,
    /// The quiet period a steer re-arms; the pinned default when absent.
    pub wake_deadline_ms: Option<i64>,
    /// `memory.recall.sidecar_max_tokens`; the pinned default when absent.
    pub sidecar_max_tokens: Option<i64>,
    /// `memory.recall.event_caps`; the stream defaults when absent.
    pub event_caps: Option<KibitzerEventCaps>,
    /// The injectable timers; the runtime's unref'd timers when absent.
    pub timers: Option<Arc<dyn KibitzerSidecarTimers>>,
    /// The clock; the system clock when absent.
    pub now: Option<Arc<dyn Fn() -> i64 + Send + Sync>>,
    /// The jitter source; a system-clock-derived source when absent.
    pub random: Option<Arc<dyn Fn() -> f64 + Send + Sync>>,
    /// The wake report callback; absent means the outcome is not observed (`onWake`).
    pub on_wake: Option<Arc<dyn Fn(KibitzerWakeOutcome) + Send + Sync>>,
    /// `options.logger?.warn`; absent means the sidecar stays silent.
    pub warn: Option<Arc<dyn Fn(&str) + Send + Sync>>,
}

/// One wake-eligible batch offered to the sidecar (upstream `KibitzerOfferInput`). The composition
/// builds this from the candidate collector's `CollectedRecallCandidates` plus the resolved task line.
pub struct KibitzerOfferInput {
    pub candidates: Vec<RecallCandidate>,
    /// Already-surfaced paths, merged before the wake policy runs.
    pub surfaced: BTreeSet<String>,
    /// `memory.recall.max_items`: how many nudges this wake may accept.
    pub max_items: usize,
    /// An explicit one-line task summary; absent keeps the remembered one.
    pub task_summary: Option<String>,
}

/// The resident sidecar (upstream `KibitzerSidecar`): the state record, the wake transitions and the
/// synchronous hook capture view. The admission is owned by the wake transitions after construction,
/// so it is NOT retained here.
pub struct KibitzerSidecar {
    core: Arc<SidecarCore>,
    turns: Arc<TurnLifecycle>,
    recovery: Arc<KibitzerRecovery>,
    wake: Arc<WakeTransitions>,
}

impl KibitzerSidecar {
    /// Builds one sidecar: the core, the turn lifecycle, the recovery, the admission and the wake
    /// transitions, wired with the injected seams. Construction creates NO child.
    pub fn new(options: KibitzerSidecarOptions) -> Arc<Self> {
        let core = create_sidecar_core(KibitzerSidecarCoreOptions {
            session_id: options.session_id.clone(),
            resources: options.resources.clone(),
            tool_budget: options.tool_budget,
            wake_deadline_ms: options.wake_deadline_ms,
            sidecar_max_tokens: options.sidecar_max_tokens,
            event_caps: options.event_caps,
            now: options.now.clone(),
            random: options.random.clone(),
            timers: options.timers.clone(),
            warn: options.warn.clone(),
        });
        let on_wake = options
            .on_wake
            .clone()
            .unwrap_or_else(|| Arc::new(|_outcome: KibitzerWakeOutcome| {}));
        let turns = Arc::new(create_turn_lifecycle(Arc::clone(&core), options.spawn.clone(), on_wake));
        let recovery = Arc::new(create_recovery(Arc::clone(&core), options.spawn.clone()));
        // The admission is consumed by the wake transitions; it is NOT retained on the sidecar.
        let admission = Arc::new(create_admission(
            Arc::clone(&core),
            Arc::clone(&options.wake_slot),
            Arc::clone(&turns),
            Arc::clone(&recovery),
            options.executor.clone(),
        ));
        let wake = create_wake_transitions(
            Arc::clone(&core),
            Arc::clone(&turns),
            admission,
            Arc::clone(&recovery),
            options.spawner.clone(),
            options.spawn.clone(),
            options.deliver.clone(),
        );
        Arc::new(Self { core, turns, recovery, wake })
    }

    pub fn session_id(&self) -> &str {
        &self.core.session_id
    }

    pub fn state(&self) -> KibitzerSidecarState {
        self.core.state()
    }

    /// Resolves once every queued transition has run AND the running turn, if any, has settled.
    pub async fn when_idle(&self) {
        self.core.when_idle().await;
    }

    /// `offer`: run the wake policy and route a wake. The whole body runs inside `core.serialized`,
    /// so two offers never interleave. `Err` is upstream's thrown invariant (`turn_running` without a
    /// live turn/child); it never happens in correct operation and is surfaced, not swallowed.
    pub async fn offer(&self, input: KibitzerOfferInput) -> Result<KibitzerOfferResult, String> {
        // An INDEPENDENT chain handle: `core` is moved into the closure, so the receiver borrow and
        // the moved Arc must not be the same binding.
        let chain = Arc::clone(&self.core);
        let core = Arc::clone(&self.core);
        let wake = Arc::clone(&self.wake);
        chain
            .serialized(move || async move {
                if core.is_closing() || core.is_disposed() {
                    return Ok(KibitzerOfferResult::Buffered { reason: KibitzerBufferedReason::Disposed });
                }
                // Merge the supplied surfaced paths before the policy runs.
                {
                    let mut surfaced = core.resources.surfaced.lock().unwrap_or_else(PoisonError::into_inner);
                    for path in &input.surfaced {
                        surfaced.insert(path.clone());
                    }
                }
                let state = core.state();
                if state == KibitzerSidecarState::Backoff {
                    return Ok(KibitzerOfferResult::Buffered { reason: KibitzerBufferedReason::Backoff });
                }
                let decision = {
                    let offered = core.resources.offered.lock().unwrap_or_else(PoisonError::into_inner).clone();
                    let surfaced = core.resources.surfaced.lock().unwrap_or_else(PoisonError::into_inner).clone();
                    let cooldown_exhausted = core.cooldown.lock().unwrap_or_else(PoisonError::into_inner).exhausted();
                    decide_wake(&input.candidates, &offered, &surfaced, input.max_items, cooldown_exhausted)
                };
                let fresh = match decision {
                    WakeDecision::Wake(fresh) => fresh,
                    WakeDecision::Silent(reason) => {
                        return Ok(KibitzerOfferResult::Buffered { reason: silence_reason(reason) });
                    }
                };
                if let Some(summary) = input.task_summary {
                    core.record.lock().unwrap_or_else(PoisonError::into_inner).task_summary = Some(summary);
                }
                // Snapshot the child and the active turn out of the record (a `Child` clone shares
                // every `Arc` field), so no record lock is held across the wake `.await`.
                let (child, turn) = {
                    let record = core.record.lock().unwrap_or_else(PoisonError::into_inner);
                    (record.child.clone(), record.active_turn.clone())
                };
                match state {
                    KibitzerSidecarState::TurnRunning => {
                        let (Some(child), Some(turn)) = (child, turn) else {
                            return Err(
                                "kibitzer sidecar invariant broken: turn_running without a live turn".to_string(),
                            );
                        };
                        Ok(wake.steer(&child, &turn, &fresh, input.max_items).await)
                    }
                    KibitzerSidecarState::Idle => match child {
                        None => Ok(wake.seed(&fresh, input.max_items).await),
                        Some(child) => Ok(wake.follow_up(Arc::clone(&child.handle), &fresh, input.max_items).await),
                    },
                    KibitzerSidecarState::Reseeding => Ok(wake.seed(&fresh, input.max_items).await),
                    // `closing`/`disposed` and `backoff` were handled above.
                    KibitzerSidecarState::Backoff | KibitzerSidecarState::Disposed => {
                        Ok(KibitzerOfferResult::Buffered { reason: KibitzerBufferedReason::Disposed })
                    }
                }
            })
            .await
    }

    /// `requestShutdown`: the SYNCHRONOUS half of shutdown. `async fn shutdown` starts only when
    /// polled, so a registered synchronous shutdown hook that merely schedules the future would not
    /// set `closing`/abort admission until the executor gets to it - a late offer could then start a
    /// turn on a shutting-down session. This method sets `closing` and aborts a parked admission
    /// IMMEDIATELY, using the existing core methods; the consumer MUST call it synchronously BEFORE
    /// scheduling the async `shutdown` cleanup. Idempotent.
    pub fn request_shutdown(&self) {
        // Before the chain: an offer parked on the wake lease must let go now, not after its wait.
        self.core.mark_closing();
        self.core.abort_admission();
    }

    /// `shutdown`: request shutdown synchronously, then queue the cleanup on the chain - settle the
    /// live turn inline (never depending on the aborted turn reporting back) and dispose terminally.
    pub async fn shutdown(&self) {
        self.request_shutdown();
        // An INDEPENDENT chain handle: `core` is moved into the closure (same reason as `offer`).
        let chain = Arc::clone(&self.core);
        let core = Arc::clone(&self.core);
        let turns = Arc::clone(&self.turns);
        let recovery = Arc::clone(&self.recovery);
        let wake = Arc::clone(&self.wake);
        chain
            .serialized(move || async move {
                if core.is_disposed() {
                    return;
                }
                recovery.clear_backoff();
                let (child, turn) = {
                    let record = core.record.lock().unwrap_or_else(PoisonError::into_inner);
                    (record.child.clone(), record.active_turn.clone())
                };
                if let (Some(child), Some(turn)) = (child, turn) {
                    *turn.abort.lock().unwrap_or_else(PoisonError::into_inner) = Some(KibitzerWakeAbort::Shutdown);
                    turns.clear_deadline(&turn);
                    turns.abort_handle(Arc::clone(&child.handle), KibitzerWakeAbort::Shutdown).await;
                    // Settle inline: shutdown must not depend on the aborted turn ever reporting back.
                    wake.settle_transition(
                        turn,
                        JudgeSettle { completed: false, cancelled: true, failure_message: None },
                    )
                    .await;
                }
                recovery.dispose_child();
                {
                    let mut record = core.record.lock().unwrap_or_else(PoisonError::into_inner);
                    record.active_turn = None;
                    record.pending_reseed = None;
                    record.carry.clear();
                    record.state = KibitzerSidecarState::Disposed;
                }
            })
            .await;
    }

    /// `events.onBranch`: the newest assistant text the branch revealed, emitted BEFORE the prompt /
    /// tool event that follows it (upstream emits it once, ahead of the hook). Call this synchronously
    /// with the captured branch entries and cursor BEFORE `on_prompt` / `on_tool_call` /
    /// `on_tool_result`. The cursor is the captured branch length (`entries.len()`). False once disposed.
    pub fn on_branch(&self, entries: &[Value], cursor: usize) -> bool {
        if self.core.is_disposed() {
            return false;
        }
        self.core
            .stream
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .newest_assistant(entries, cursor)
    }

    /// `events.onPrompt`: remember the task line, then buffer the prompt. False once disposed.
    pub fn on_prompt(&self, prompt: &str, cursor: usize) -> bool {
        if self.core.is_disposed() {
            return false;
        }
        self.remember_task(prompt);
        self.core.stream.lock().unwrap_or_else(PoisonError::into_inner).on_prompt(prompt, cursor)
    }

    /// `events.onToolCall`: buffer a tool call. False once disposed.
    pub fn on_tool_call(&self, tool_name: &str, args: &Value, cursor: usize) -> bool {
        if self.core.is_disposed() {
            return false;
        }
        self.core
            .stream
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .on_tool_call(tool_name, args, cursor)
    }

    /// `events.onToolResult`: buffer a tool result. False once disposed.
    pub fn on_tool_result(&self, tool_name: &str, result: &Value, is_error: bool, cursor: usize) -> bool {
        if self.core.is_disposed() {
            return false;
        }
        self.core
            .stream
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .on_tool_result(tool_name, result, is_error, cursor)
    }

    /// `events.size`: buffered (unfolded) events.
    pub fn event_size(&self) -> usize {
        self.core.stream.lock().unwrap_or_else(PoisonError::into_inner).size()
    }

    /// `events.lastCursor`: the newest parent cursor the stream has seen, if any.
    pub fn last_cursor(&self) -> Option<usize> {
        self.core.stream.lock().unwrap_or_else(PoisonError::into_inner).last_cursor()
    }

    /// `rememberTask`: the FIRST non-empty prompt becomes the whitespace-normalized task summary,
    /// capped at `TASK_SUMMARY_HEAD_CHARS` UTF-16 units; later prompts do not overwrite it.
    fn remember_task(&self, prompt: &str) {
        {
            let record = self.core.record.lock().unwrap_or_else(PoisonError::into_inner);
            if record.task_summary.is_some() {
                return;
            }
        }
        let line = collapse_whitespace(prompt);
        if line.is_empty() {
            return;
        }
        let capped = truncate_utf16(&line, TASK_SUMMARY_HEAD_CHARS);
        self.core.record.lock().unwrap_or_else(PoisonError::into_inner).task_summary = Some(capped);
    }
}

/// The applied `WakeSilenceReason` → the offer result's buffered reason (upstream maps each silence).
fn silence_reason(reason: WakeSilenceReason) -> KibitzerBufferedReason {
    match reason {
        WakeSilenceReason::MaxItemsZero => KibitzerBufferedReason::MaxItemsZero,
        WakeSilenceReason::NoCandidates => KibitzerBufferedReason::NoCandidates,
        WakeSilenceReason::NoNewCandidate => KibitzerBufferedReason::NoNewCandidate,
        WakeSilenceReason::Cooldown => KibitzerBufferedReason::Cooldown,
    }
}

/// JS `\s`: exactly the code points `String.prototype.replace(/\s+/g, " ")` collapses. `U+FEFF` IS
/// whitespace; `U+0085` is NOT. Rust's `char::is_whitespace` differs on both, so it is not used.
fn is_js_whitespace(character: char) -> bool {
    matches!(
        character,
        '\u{0009}'..='\u{000d}'
            | '\u{0020}'
            | '\u{00a0}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200a}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202f}'
            | '\u{205f}'
            | '\u{3000}'
            | '\u{feff}'
    )
}

/// `text.replace(/\s+/g, " ").trim()`: every run of JS whitespace collapses to one space, then the
/// ends are trimmed.
fn collapse_whitespace(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut pending_space = false;
    for character in text.chars() {
        if is_js_whitespace(character) {
            pending_space = !out.is_empty();
            continue;
        }
        if pending_space {
            out.push(' ');
            pending_space = false;
        }
        out.push(character);
    }
    out
}

/// `line.length > cap ? line.slice(0, cap) : line` in UTF-16 units (JS `String.length`).
fn truncate_utf16(text: &str, cap: usize) -> String {
    let units: Vec<u16> = text.encode_utf16().collect();
    if units.len() <= cap {
        return text.to_string();
    }
    String::from_utf16_lossy(&units[..cap])
}
