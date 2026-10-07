//! One wake as a tracked turn (latest `kibitzer/sidecar-turn.ts`).
//!
//! A wake is one child turn: its record, the child's own event stream read as the sidecar's
//! instrument (tool calls against the budget, which steers reached the transcript, provider usage
//! for the context estimate), the deadline timer, the sidecar's own abort, and the outcome report.
//! Starting and settling a turn is `sidecar-wake`'s business; this module never changes
//! `SidecarRecord.state`.
//!
//! # Injected seams (the two the core options cannot carry)
//!
//! Upstream `createTurnLifecycle(core)` reads `core.options.onWake` and leans on the ambient JS
//! microtask queue for its fire-and-forget `void abortTurn(...)`. The applied
//! `KibitzerSidecarCoreOptions` carries neither: it has no `on_wake` field, and the crate assumes no
//! ambient runtime. So [`create_turn_lifecycle`] takes the two seams explicitly, and the public
//! composition must forward the real ones - `spawn`, the host executor every event-triggered async
//! transition is scheduled on (the same seam `ResidentKibitzerRunner` holds), and `on_wake`, the
//! report callback `report` invokes outside every lock. No core edit is made or required.
//!
//! # Deviations (each an N/A, because the Rust crate has no direct counterpart)
//!
//! * `ChildSessionEvent`'s three arms are the applied [`KibitzerChildObservation`] arms, and its
//!   `message_end` payload is the native `AgentMessage`: `observe_message` reads role, the
//!   `UserContent` text and the assistant's real `Usage` from that typed contract instead of the
//!   upstream `isRecord`/`numberOf` duck-typing, so no optional or non-finite usage case exists.
//! * The child's `tools.searchedPaths` is not on `Child`; the port unifies it into the shared
//!   `resources.searched` set, so `validated` reads that handle and keeps `current` only for the
//!   upstream call shape.
//! * `abortHandle` awaited `handle.abort()` and reported a rejection through `core.warn`; the
//!   applied `KibitzerChild::abort` returns `()`, so there is no rejection to report and `cause`
//!   stays in the signature for the upstream call shape.
//! * `void abortTurn(...)` (fire-and-forget) becomes an explicit [`KibitzerWakeSpawn`] scheduling of
//!   the serialized transition, never a dropped future; the per-session chain is the async
//!   `serialized` mutex, so no std guard is held across an `.await`.
//! * Upstream `newTurn` seeds `settled` with an already-resolved promise that `beginTurn` replaces;
//!   the applied `Turn.settled` is a fixed `Arc<Settlement>` (no replaceable field, and no core edit
//!   is made), so `new_turn` leaves it UNRESOLVED and the wake transition resolves it when the child
//!   turn ends. An admission-refusal turn (`sidecar-admission` `refused` -> `newTurn` + `report`)
//!   never becomes `record.active_turn`, so its unsettled `Settlement` is never awaited and
//!   `when_idle` cannot hang on it. N/A: a fixed field cannot be pre-resolved then replaced.
//! * `report`'s `try { onWake } catch` becomes `catch_unwind` around the callback (the crate has no
//!   throwing callback ABI) and still warns through `core.warn`, outside every lock.
//! * Upstream's `tool_execution_end` arm stops only on the count (`toolEnds >= budget`). The applied
//!   observation carries the registered tool result's own metadata, so the end also stops on a REAL
//!   cap result - `terminate` set, or the `tool_budget_exceeded` refusal code - with the same
//!   `ToolBudget` cause and no second charge. This is the recorded registered-path obligation, not a
//!   silent change; the count rule is preserved and a below-cap argument rejection still continues.
//! * `arm_deadline` gives each arm its OWNING token BEFORE `timers.set` (never a handle read back
//!   from `set`), and every `clear`/rearm invalidates the prior token synchronously, so a callback
//!   that fired before its arm finished registering - or one paused past a rearm or a clear - can
//!   never abort the current turn. The token lives on a demanded per-turn `Turn.deadline_epoch`
//!   field; see the producer demand in the receipt. `clear` invalidates the token BEFORE it clears
//!   the timer, exactly as `sidecar-recovery` re-checks its owning state before acting.
//! * `Turn.candidate_count` is an `AtomicUsize` in the applied core (the steer transition increments
//!   it as it absorbs a batch), so `new_turn` seeds it with `AtomicUsize::new(candidate_count)` and
//!   `report` reads it with `.load(Ordering::SeqCst)`. N/A: upstream's `turn.candidateCount += ...`
//!   needs a mutable field, which an `Arc<Turn>` shared with the wake owner cannot offer.

use std::sync::atomic::{AtomicI64, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use serde_json::Value;

use memory_core::recall::{RecallCandidate, RecallNudge, ValidateNudgesOptions, validate_nudges};

use maho_ext_api::AgentMessage;

use crate::kibitzer_child::{KibitzerChild, KibitzerChildObservation};
use crate::kibitzer_contract::{KIBITZER_WAKE_MAX_TOTAL_MS, KibitzerWakeSpawn};
use crate::kibitzer_sidecar_core::{Child, Settlement, SidecarCore, Turn, text_of};
use crate::kibitzer_sidecar_outcome::{
    KibitzerCursorSpan, KibitzerWakeAbort, KibitzerWakeEnd, KibitzerWakeOutcome, KibitzerWakeUsage,
    is_diagnostic_wake_end,
};
use crate::kibitzer_tools_result::KibitzerRejectionCode;

/// The per-wake lifecycle of one resident sidecar (upstream `TurnLifecycle`).
///
/// Upstream models it as an `interface` returned by `createTurnLifecycle`; the port keeps the one
/// concrete object as a struct, because it is an internal module object and not an injected seam
/// (unlike `KibitzerChild` or `KibitzerWakeRunner`). `core` is the shared state record; `spawn` and
/// `on_wake` are the two seams the core options cannot carry (see the module docs).
pub struct TurnLifecycle {
    core: Arc<SidecarCore>,
    spawn: KibitzerWakeSpawn,
    on_wake: Arc<dyn Fn(KibitzerWakeOutcome) + Send + Sync>,
}

/// Builds the per-wake turn lifecycle (upstream `createTurnLifecycle`).
pub fn create_turn_lifecycle(
    core: Arc<SidecarCore>,
    spawn: KibitzerWakeSpawn,
    on_wake: Arc<dyn Fn(KibitzerWakeOutcome) + Send + Sync>,
) -> TurnLifecycle {
    TurnLifecycle { core, spawn, on_wake }
}

impl TurnLifecycle {
    /// The next wake number with fresh accepted/budget slots; not yet the active turn. The wake
    /// transition sets `record.active_turn` and `record.state` itself, and resolves the turn's
    /// settlement when the child turn ends. The returned turn's `settled` starts UNRESOLVED: an
    /// admission-refusal caller (`sidecar-admission` `refused`) that only reports a start failure
    /// never becomes `active_turn`, so nothing awaits it.
    pub fn new_turn(&self, generation: u64, max_items: usize, candidate_count: usize) -> Arc<Turn> {
        let wake = {
            let mut record = self.core.record.lock().unwrap_or_else(PoisonError::into_inner);
            record.wake_seq += 1;
            record.wake_seq
        };
        self.core.resources.accepted.reset();
        self.core.resources.budget_slot.reset(self.core.tool_budget);
        let accepted = self.core.resources.accepted.current();
        let budget = self.core.resources.budget_slot.current();
        let started_at = (self.core.now)();
        Arc::new(Turn {
            wake,
            generation,
            max_items,
            started_at,
            total_deadline_at: started_at + KIBITZER_WAKE_MAX_TOTAL_MS,
            accepted,
            budget,
            envelopes: Mutex::new(Vec::new()),
            lease: Mutex::new(None),
            slot_wait_ms: AtomicI64::new(0),
            candidate_count: AtomicUsize::new(candidate_count),
            tool_starts: AtomicUsize::new(0),
            tool_ends: AtomicUsize::new(0),
            abort: Mutex::new(None),
            deadline: Mutex::new(None),
            deadline_epoch: AtomicU64::new(0),
            model: Mutex::new(None),
            usage: Mutex::new(None),
            settled: Settlement::new(),
        })
    }

    /// Marks the candidates as offered to this sidecar lifetime, at the given wake.
    pub fn offer_paths(&self, candidates: &[RecallCandidate], wake: u64) {
        for candidate in candidates {
            self.core
                .resources
                .offered
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(candidate.path.clone());
            self.core
                .record
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .offered_at_wake
                .insert(candidate.path.clone(), wake);
        }
    }

    /// The child's subscription: budget, consumption, usage. Only the active turn is observed.
    pub fn observe(&self, observation: KibitzerChildObservation) {
        let turn = {
            self.core.record.lock().unwrap_or_else(PoisonError::into_inner).active_turn.clone()
        };
        let Some(turn) = turn else { return };
        match observation {
            KibitzerChildObservation::ToolStart { .. } => {
                // A parallel batch can start a call past the budget before the eighth one ends.
                let starts = turn.tool_starts.fetch_add(1, Ordering::SeqCst) + 1;
                if starts > self.core.tool_budget {
                    schedule_abort(&self.spawn, &self.core, &turn, KibitzerWakeAbort::ToolBudget);
                }
            }
            KibitzerChildObservation::ToolEnd { terminate, refusal, .. } => {
                let ends = turn.tool_ends.fetch_add(1, Ordering::SeqCst) + 1;
                // Upstream stops at the budget by count; the registered path also reports an actual
                // cap result - `terminate` set, or the `tool_budget_exceeded` refusal code - which
                // stops the same way. Neither branch charges the budget again (the observer only
                // counts), and a below-cap argument rejection (`terminate` false, a different code)
                // is left to continue.
                let refused_at_cap = terminate
                    || refusal.as_deref() == Some(KibitzerRejectionCode::ToolBudgetExceeded.as_str());
                if ends >= self.core.tool_budget || refused_at_cap {
                    schedule_abort(&self.spawn, &self.core, &turn, KibitzerWakeAbort::ToolBudget);
                }
            }
            KibitzerChildObservation::MessageEnd { message } => self.observe_message(&turn, &message),
        }
    }

    /// A user message marks only the first unconsumed exactly matching envelope; an assistant
    /// message accumulates provider/model and usage and refreshes the child's context estimate.
    fn observe_message(&self, turn: &Turn, message: &AgentMessage) {
        match message.role() {
            "user" => {
                let text = message_text(message);
                let mut envelopes = turn.envelopes.lock().unwrap_or_else(PoisonError::into_inner);
                if let Some(envelope) =
                    envelopes.iter_mut().find(|envelope| !envelope.consumed && envelope.text == text)
                {
                    envelope.consumed = true;
                }
            }
            "assistant" => {
                let Some(assistant) = message.as_assistant() else { return };
                {
                    let mut model = turn.model.lock().unwrap_or_else(PoisonError::into_inner);
                    *model = Some(format!("{}/{}", assistant.provider, assistant.model));
                }
                let usage = assistant.usage;
                {
                    let mut current = turn.usage.lock().unwrap_or_else(PoisonError::into_inner);
                    let previous = (*current).unwrap_or(KibitzerWakeUsage {
                        input: 0,
                        output: 0,
                        cache_read: 0,
                        cache_write: 0,
                    });
                    *current = Some(KibitzerWakeUsage {
                        input: previous.input + usage.input as i64,
                        output: previous.output + usage.output as i64,
                        cache_read: previous.cache_read + usage.cache_read as i64,
                        cache_write: previous.cache_write + usage.cache_write as i64,
                    });
                }
                let record = self.core.record.lock().unwrap_or_else(PoisonError::into_inner);
                if let Some(child) = record.child.as_ref() {
                    *child.usage_tokens.lock().unwrap_or_else(PoisonError::into_inner) =
                        Some(usage.input as i64 + usage.cache_read as i64);
                }
            }
            _ => {}
        }
    }

    /// `usage.input + usage.cacheRead` of the newest assistant message, char/4 fallback.
    pub fn context_estimate(&self, current: &Child) -> i64 {
        if let Some(tokens) = *current.usage_tokens.lock().unwrap_or_else(PoisonError::into_inner) {
            return tokens;
        }
        current.chars_sent.lock().unwrap_or_else(PoisonError::into_inner).div_ceil(4)
    }

    /// Arms the wake deadline at `min(now + wakeDeadlineMs, turn.totalDeadlineAt)`: the quiet period
    /// every steer re-arms, clamped by the wake's own cap from admission. A supplied `on_expire`
    /// replaces what a fired deadline does - `seed` and `followUp` arm it around the child I/O they
    /// bound, where there is no running turn to abort yet.
    pub fn arm_deadline(&self, turn: &Arc<Turn>, on_expire: Option<Box<dyn FnOnce() + Send>>) {
        self.clear_deadline(turn);
        let quiet_ms = self
            .core
            .wake_deadline_ms
            .min((turn.total_deadline_at - (self.core.now)()).max(0));
        // The OWNING token of THIS arm, taken BEFORE `timers.set` and never read back from the
        // handle `set` returns (which is only known after the callback is built). `clear_deadline`
        // above already invalidated every earlier arm; this fetch establishes the current one. A
        // superseded callback compares its captured token against the turn's live token and does
        // nothing when they differ, so a callback that fired before this arm finished registering -
        // or one paused past a rearm or a clear - can never abort the current turn.
        let token = turn.deadline_epoch.fetch_add(1, Ordering::SeqCst) + 1;
        let guard_turn = Arc::clone(turn);
        let callback: Box<dyn FnOnce() + Send> = match on_expire {
            Some(expire) => Box::new(move || {
                if is_current_deadline(&guard_turn, token) {
                    expire();
                }
            }),
            None => {
                let core = Arc::clone(&self.core);
                let spawn = Arc::clone(&self.spawn);
                let turn = Arc::clone(turn);
                Box::new(move || {
                    if is_current_deadline(&guard_turn, token) {
                        schedule_abort(&spawn, &core, &turn, KibitzerWakeAbort::Deadline);
                    }
                })
            }
        };
        let handle = self.core.timers.set(callback, quiet_ms);
        *turn.deadline.lock().unwrap_or_else(PoisonError::into_inner) = Some(handle);
    }

    /// Clears the armed deadline timer, if any.
    pub fn clear_deadline(&self, turn: &Turn) {
        clear_deadline_impl(&self.core, turn);
    }

    /// The sidecar's own abort: recorded first so settlement reads the cause, not the engine's
    /// `cancelled`. Runs as the next serialized transition.
    pub async fn abort_turn(&self, turn: Arc<Turn>, cause: KibitzerWakeAbort) {
        abort_transition(Arc::clone(&self.core), turn, cause).await;
    }

    /// Signals the native child to abort. Kept as the upstream call shape; see the module docs.
    pub async fn abort_handle(&self, handle: Arc<dyn KibitzerChild>, cause: KibitzerWakeAbort) {
        abort_handle_impl(&handle, cause).await;
    }

    /// Defence in depth over the closure's call-time checks: the lifetime allowed set
    /// (`offered` union `searched`) minus the session ledger, capped at the turn's `max_items`.
    pub fn validated(&self, turn: &Turn, current: &Child) -> Vec<RecallNudge> {
        // The child's `searchedPaths` is the shared `resources.searched` set in this port; `current`
        // is kept only so the upstream call shape translates mechanically.
        let _ = current;
        let mut candidates = self
            .core
            .resources
            .offered
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        candidates.extend(
            self.core
                .resources
                .searched
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .iter()
                .cloned(),
        );
        let surfaced = self
            .core
            .resources
            .surfaced
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let accepted = turn.accepted.lock().unwrap_or_else(PoisonError::into_inner).clone();
        let options = ValidateNudgesOptions { candidates, surfaced, max_items: turn.max_items };
        validate_nudges(&accepted, &options)
    }

    /// The closed `KibitzerWakeOutcome` for a settled wake, handed to `on_wake` outside every lock.
    pub fn report(
        &self,
        turn: &Turn,
        end: &KibitzerWakeEnd,
        nudges: &[RecallNudge],
        current: Option<&Child>,
        model: Option<String>,
    ) {
        let cursors = cursors_span(turn);
        let provenance = model
            .or_else(|| turn.model.lock().unwrap_or_else(PoisonError::into_inner).clone());
        let (reason, configuration) = match end {
            KibitzerWakeEnd::Failed { reason, configuration, .. } => {
                (reason.clone(), configuration.clone())
            }
            _ => (None, None),
        };
        let steered = turn
            .envelopes
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .filter(|envelope| envelope.steered)
            .count();
        let outcome = KibitzerWakeOutcome {
            session_id: self.core.session_id.clone(),
            wake: turn.wake,
            generation: turn.generation,
            status: end.status(),
            cause: end.cause(),
            reason,
            configuration,
            model: provenance,
            nudges: nudges.to_vec(),
            candidate_count: turn.candidate_count.load(Ordering::SeqCst),
            steered,
            tool_calls: turn.tool_starts.load(Ordering::SeqCst),
            duration_ms: ((self.core.now)() - turn.started_at).max(0),
            slot_wait_ms: turn.slot_wait_ms.load(Ordering::SeqCst),
            cursors,
            context_tokens: current.map(|child| self.context_estimate(child)),
            usage: *turn.usage.lock().unwrap_or_else(PoisonError::into_inner),
            diagnostic: is_diagnostic_wake_end(end),
        };
        let wake = turn.wake;
        let reported =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (self.on_wake)(outcome)));
        if let Err(payload) = reported {
            self.core.warn(
                "omo-senpi kibitzer sidecar wake report failed",
                Some(serde_json::json!({ "wake": wake, "error": panic_text(&*payload) })),
            );
        }
    }
}

/// Upstream `textOf(message.content)` over the native message: a string is itself, and an array
/// contributes only its `type: "text"` blocks, concatenated with no separator.
fn message_text(message: &AgentMessage) -> String {
    let value = serde_json::to_value(message).unwrap_or(Value::Null);
    text_of(value.get("content").unwrap_or(&Value::Null))
}

/// The parent cursor span of the events a wake carried, or `None` when no envelope carried one.
fn cursors_span(turn: &Turn) -> Option<KibitzerCursorSpan> {
    turn.envelopes
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .filter_map(|envelope| envelope.payload.cursors)
        .reduce(|span, range| KibitzerCursorSpan {
            first: span.first.min(range.first),
            last: span.last.max(range.last),
        })
}

/// Schedules the serialized abort transition on the retained host executor, never dropping it.
fn schedule_abort(
    spawn: &KibitzerWakeSpawn,
    core: &Arc<SidecarCore>,
    turn: &Arc<Turn>,
    cause: KibitzerWakeAbort,
) {
    let core = Arc::clone(core);
    let turn = Arc::clone(turn);
    spawn(Box::pin(async move {
        abort_transition(core, turn, cause).await;
    }));
}

/// Upstream `abortTurn`: the cause is recorded first, the deadline cleared, then the native child is
/// signalled - all as one serialized transition. A replaced or already-aborted turn is left alone,
/// and a turn with no child records nothing.
async fn abort_transition(core: Arc<SidecarCore>, turn: Arc<Turn>, cause: KibitzerWakeAbort) {
    core.serialized({
        let core = Arc::clone(&core);
        let turn = Arc::clone(&turn);
        move || async move {
            let is_active = {
                let record = core.record.lock().unwrap_or_else(PoisonError::into_inner);
                record.active_turn.as_ref().is_some_and(|active| Arc::ptr_eq(active, &turn))
            };
            let already = turn.abort.lock().unwrap_or_else(PoisonError::into_inner).is_some();
            let handle = {
                let record = core.record.lock().unwrap_or_else(PoisonError::into_inner);
                record.child.as_ref().map(|child| Arc::clone(&child.handle))
            };
            let Some(handle) = handle else { return };
            if !is_active || already {
                return;
            }
            *turn.abort.lock().unwrap_or_else(PoisonError::into_inner) = Some(cause);
            clear_deadline_impl(&core, &turn);
            abort_handle_impl(&handle, cause).await;
        }
    })
    .await;
}

/// Upstream `abortHandle`: signals the native child to abort. `KibitzerChild::abort` returns `()`,
/// so there is no rejection to report through `warn`; `cause` stays for the upstream call shape.
async fn abort_handle_impl(handle: &Arc<dyn KibitzerChild>, cause: KibitzerWakeAbort) {
    let _ = cause;
    handle.abort();
}

/// True when the deadline callback that fired still owns the turn's CURRENT deadline token: the
/// turn's live epoch equals the token the arm captured before `timers.set`. A rearm or a clear has
/// already advanced the epoch, so a superseded callback sees a mismatch and does nothing.
fn is_current_deadline(turn: &Turn, token: u64) -> bool {
    turn.deadline_epoch.load(Ordering::SeqCst) == token
}

/// Upstream `clearDeadline`: invalidates the armed callback's token FIRST, then clears the timer
/// once and forgets the handle. A callback already past `timers.clear` (which cannot unschedule it)
/// still sees the advanced epoch and does nothing.
fn clear_deadline_impl(core: &SidecarCore, turn: &Turn) {
    turn.deadline_epoch.fetch_add(1, Ordering::SeqCst);
    let handle = turn.deadline.lock().unwrap_or_else(PoisonError::into_inner).take();
    if let Some(handle) = handle {
        core.timers.clear(handle);
    }
}

/// The message of a `catch_unwind` payload, matching upstream `describe(error)` for the report warn.
fn panic_text(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        (*message).to_string()
    } else if let Some(message) = payload.downcast_ref::<String>() {
        message.clone()
    } else {
        "non-string panic payload".to_string()
    }
}
#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use crate::kibitzer_contract::{
        KIBITZER_WAKE_MAX_TOTAL_MS, KibitzerSidecarTimers, KibitzerWakeSpawn,
    };
    use crate::kibitzer_sidecar_core::{KibitzerSidecarCoreOptions, Turn, create_sidecar_core};
    use crate::kibitzer_sidecar_outcome::KibitzerWakeOutcome;
    use crate::kibitzer_session_resources::KibitzerSessionResourceRegistry;

    use super::{TurnLifecycle, create_turn_lifecycle};

    const NOW_MS: i64 = 1_700_000_000_000;

    struct Queued {
        ms: i64,
        callback: Option<Box<dyn FnOnce() + Send>>,
        cleared: bool,
    }

    /// A fake [`KibitzerSidecarTimers`] that never sleeps and never spawns. In QUEUED mode `set`
    /// stores the callback and returns a handle, and `fire` runs it by hand; in IMMEDIATE mode `set`
    /// runs the callback synchronously BEFORE returning, exactly as a timer that is already due
    /// would. `clear` marks the handle cleared but KEEPS the callback body, so a test can fire one
    /// that already began - the "a running callback cannot be unscheduled" case.
    struct FakeTimers {
        next: AtomicU64,
        immediate: AtomicBool,
        queued: Mutex<BTreeMap<u64, Queued>>,
    }

    impl FakeTimers {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                next: AtomicU64::new(0),
                immediate: AtomicBool::new(false),
                queued: Mutex::new(BTreeMap::new()),
            })
        }

        fn ms_of(&self, handle: u64) -> i64 {
            self.queued.lock().unwrap().get(&handle).expect("the arm is queued").ms
        }

        fn is_cleared(&self, handle: u64) -> bool {
            self.queued.lock().unwrap().get(&handle).expect("the arm is queued").cleared
        }

        fn fire(&self, handle: u64) {
            let callback = self
                .queued
                .lock()
                .unwrap()
                .get_mut(&handle)
                .and_then(|queued| queued.callback.take());
            if let Some(callback) = callback {
                callback();
            }
        }
    }

    impl KibitzerSidecarTimers for FakeTimers {
        fn set(&self, callback: Box<dyn FnOnce() + Send>, ms: i64) -> u64 {
            let handle = self.next.fetch_add(1, Ordering::SeqCst) + 1;
            if self.immediate.load(Ordering::SeqCst) {
                callback();
                return handle;
            }
            self.queued
                .lock()
                .unwrap()
                .insert(handle, Queued { ms, callback: Some(callback), cleared: false });
            handle
        }

        fn clear(&self, handle: u64) {
            if let Some(queued) = self.queued.lock().unwrap().get_mut(&handle) {
                queued.cleared = true;
            }
        }
    }

    struct Harness {
        turns: TurnLifecycle,
        timers: Arc<FakeTimers>,
        clock: Arc<AtomicI64>,
        scheduled: Arc<AtomicUsize>,
    }

    fn harness(wake_deadline_ms: i64, immediate: bool) -> Harness {
        let clock = Arc::new(AtomicI64::new(NOW_MS));
        let timers = FakeTimers::new();
        if immediate {
            timers.immediate.store(true, Ordering::SeqCst);
        }
        let scheduled = Arc::new(AtomicUsize::new(0));
        let resources = KibitzerSessionResourceRegistry::new().for_session("session");
        let now_clock = Arc::clone(&clock);
        let now: Arc<dyn Fn() -> i64 + Send + Sync> =
            Arc::new(move || now_clock.load(Ordering::SeqCst));
        let exec_counter = Arc::clone(&scheduled);
        let spawn: KibitzerWakeSpawn =
            Arc::new(move |_future: Pin<Box<dyn Future<Output = ()> + Send>>| {
                exec_counter.fetch_add(1, Ordering::SeqCst);
            });
        let timer_handle: Arc<dyn KibitzerSidecarTimers> = timers.clone();
        let core = create_sidecar_core(KibitzerSidecarCoreOptions {
            session_id: "session".to_string(),
            resources,
            tool_budget: None,
            wake_deadline_ms: Some(wake_deadline_ms),
            sidecar_max_tokens: None,
            event_caps: None,
            now: Some(now),
            random: None,
            timers: Some(timer_handle),
            warn: None,
        });
        let on_wake: Arc<dyn Fn(KibitzerWakeOutcome) + Send + Sync> =
            Arc::new(|_outcome: KibitzerWakeOutcome| {});
        let turns = create_turn_lifecycle(core, spawn, on_wake);
        Harness { turns, timers, clock, scheduled }
    }

    fn armed(turn: &Arc<Turn>) -> u64 {
        turn.deadline.lock().unwrap().expect("a deadline is armed")
    }

    fn flag_callback() -> (Arc<AtomicBool>, Box<dyn FnOnce() + Send>) {
        let flag = Arc::new(AtomicBool::new(false));
        let writer = Arc::clone(&flag);
        (flag, Box::new(move || writer.store(true, Ordering::SeqCst)))
    }

    #[test]
    fn quiet_deadline_is_clamped_to_the_total_ceiling() {
        let harness = harness(500_000, false);
        let turn = harness.turns.new_turn(1, 5, 0);

        harness.turns.arm_deadline(&turn, None);
        assert_eq!(harness.timers.ms_of(armed(&turn)), KIBITZER_WAKE_MAX_TOTAL_MS);

        harness.clock.fetch_add(250_000, Ordering::SeqCst);
        harness.turns.arm_deadline(&turn, None);
        assert_eq!(harness.timers.ms_of(armed(&turn)), KIBITZER_WAKE_MAX_TOTAL_MS - 250_000);
    }

    #[test]
    fn a_rearmed_deadline_ignores_the_old_callback() {
        let harness = harness(90_000, false);
        let turn = harness.turns.new_turn(1, 5, 0);

        let (old_fired, old_callback) = flag_callback();
        harness.turns.arm_deadline(&turn, Some(old_callback));
        let old_handle = armed(&turn);

        let (new_fired, new_callback) = flag_callback();
        harness.turns.arm_deadline(&turn, Some(new_callback));
        let new_handle = armed(&turn);

        harness.timers.fire(old_handle);
        assert!(!old_fired.load(Ordering::SeqCst));

        harness.timers.fire(new_handle);
        assert!(new_fired.load(Ordering::SeqCst));
    }

    #[test]
    fn clear_invalidates_a_queued_callback() {
        let harness = harness(90_000, false);
        let turn = harness.turns.new_turn(1, 5, 0);

        let (fired, callback) = flag_callback();
        harness.turns.arm_deadline(&turn, Some(callback));
        let handle = armed(&turn);

        harness.turns.clear_deadline(&turn);
        assert!(turn.deadline.lock().unwrap().is_none());
        assert!(harness.timers.is_cleared(handle));

        harness.timers.fire(handle);
        assert!(!fired.load(Ordering::SeqCst));
    }

    #[test]
    fn an_immediate_callback_is_valid() {
        let harness = harness(90_000, true);
        let turn = harness.turns.new_turn(1, 5, 0);

        let (fired, callback) = flag_callback();
        harness.turns.arm_deadline(&turn, Some(callback));
        assert!(fired.load(Ordering::SeqCst));

        let next = harness.turns.new_turn(2, 5, 0);
        harness.turns.arm_deadline(&next, None);
        assert_eq!(harness.scheduled.load(Ordering::SeqCst), 1);
    }
}
