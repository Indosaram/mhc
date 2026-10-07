//! Starting one wake: the `seed` transition of the resident sidecar (latest `kibitzer/sidecar-wake.ts`).
//!
//! A seed takes ONE machine-wide lease, cuts the carried and buffered events into one payload, renders
//! the first message (a seed envelope, or a reseed envelope followed by a wake envelope when a restart
//! is pending), creates the resident child, and starts its turn. The wake is bounded from admission:
//! the deadline is armed BEFORE the child I/O it bounds, so a `spawn` that never returns is cut off by
//! a transition that releases the lease and reports the wake without queueing behind that I/O.
//!
//! # Scope
//!
//! This unit ports `seed`, `followUp` and `settle` (and the private helpers they need). `steer` and
//! the public composition (`sidecar.ts`) are separate units on this same module. One symbol this
//! module CALLS is defined by another producer and is not defined in this file -
//! `crate::kibitzer_sidecar_model::kibitzer_configuration_failure` (the core owner, over the six-code
//! `KibitzerSidecarStartError`) - so this module does NOT compile standalone until that producer lands.
//! Its exact signature, the child settle-future lifetime demand, the spawn-ABI change, the CLI
//! trigger-scheduling seam and the delivery seam are recorded in the owned receipts
//! (`authoring/sidecar-wake-seed-small.md`, `authoring/sidecar-wake-settle-small.md`,
//! `authoring/sidecar-wake-followup-small.md`); this source defines no placeholder or fallback.
//!
//! # Injected seams (the constructor inputs the core options do not carry)
//!
//! Upstream `createWakeTransitions(core, turns, admission, recovery)` reads `core.options.startChild`,
//! `core.options.createTools` and `core.options.deliver`. The applied `KibitzerSidecarCoreOptions`
//! carries none of them, so this unit takes the real child spawner and the host executor EXPLICITLY,
//! exactly as `create_turn_lifecycle` and `create_recovery` take their seams. The member tools need no
//! seam: `KibitzerChildSpawnInput.tools` is the NAME list and the CLI's `KibitzerChildResourcesFactory`
//! builds the closures; the deliver seam belongs to the settle unit.
//!
//! # Deviations (each an N/A, because the Rust crate has no direct counterpart)
//!
//! * The child is spawned IDLE and its first turn is started by `KibitzerChild::follow_up(text)` (the
//!   real `CliKibitzerChild` spawner builds an idle session; the idle branch awaits the full
//!   `session.prompt`). Upstream `startChild` runs the turn itself. No `SpawnInput.prompt` field is
//!   added: the trigger is the existing `follow_up`.
//! * `void abandonPendingStart(...)` (fire-and-forget) becomes an explicit [`KibitzerWakeSpawn`]
//!   scheduling, never a dropped future.
//! * The pending-start completion/expiry race is closed with a module-local atomic claim (an explicit
//!   minimal pending state) instead of a `Turn` field, so no core edit is needed.
//! * A start failure is a typed `KibitzerSidecarStartError` (six codes); its human text is
//!   `error.message`, and the configuration classification is `kibitzer_configuration_failure(&error)`.
//!   No string classification is invented here.
//! * `settle` runs inside the per-session chain (`core.serialized`), so its `.await` of the deliver
//!   seam happens on the chain and never while a `std::sync::MutexGuard` is held.
//! * Upstream `settle`'s `outcome.model` has no field on the applied `JudgeSettle`, so the report is
//!   passed `None` and falls back to `turn.model` (recorded in the settle receipt).
//! * `followUp` installs the turn (`active_turn`, state, `chars_sent`, envelope, deadline) and arms the
//!   settlement BEFORE the trigger, unlike upstream `beginTurn` (which runs after its quick trigger):
//!   the applied native trigger awaits the whole turn, so the turn must already be active for the
//!   child's observations to be seen. The CLI trigger-scheduling seam (followup receipt) restores
//!   upstream's timing by returning once the turn starts.
//! * `report` is invoked OUTSIDE the record lock, with a `Child` snapshot cloned out of the record
//!   (its `Arc` fields are shared), because the `on_wake` callback may reenter the sidecar.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use memory_core::recall::{RecallCandidate, RecallNudge};
use serde_json::json;

use crate::kibitzer_child::{
    JudgeSettle, KibitzerChild, KibitzerChildObservation, KibitzerChildSpawnInput, KibitzerChildSpawner,
};
use crate::kibitzer_contract::{
    KibitzerBufferedReason, KibitzerOfferResult, KibitzerSidecarState, KibitzerWakeSpawn,
};
use crate::kibitzer_prompt::{
    KibitzerReseedInput, render_kibitzer_reseed_prompt, render_kibitzer_seed_prompt,
    render_kibitzer_wake_prompt,
};
use crate::kibitzer_prompt_blocks::KIBITZER_SIDECAR_TOOL_NAMES;
use crate::kibitzer_sidecar_admission::{Admission, SidecarAdmission};
use crate::kibitzer_sidecar_core::{Child, Envelope, Payload, SidecarCore, Turn};
use crate::kibitzer_sidecar_envelope::{envelope_input, merge, payload_of};
use crate::kibitzer_sidecar_model::kibitzer_configuration_failure;
use crate::kibitzer_sidecar_outcome::{
    KibitzerWakeAbort, KibitzerWakeEnd, KibitzerWakeFailureCause, classify_wake_end, start_failure_end,
};
use crate::kibitzer_sidecar_recovery::KibitzerRecovery;
use crate::kibitzer_sidecar_turn::TurnLifecycle;

/// The delivery seam `settle` awaits (upstream `core.options.deliver(nudges, { wake, generation })`).
///
/// The applied `KibitzerDelivery::accept` is SYNCHRONOUS and needs the session's
/// `MemoryIdentityContext` (composition-owned), so the composition supplies this seam over it - a
/// trivial `Pin::from(Box::pin(async move { ... }))` when `accept` is called directly. A rejection is
/// warned by `settle` and never fails the wake.
pub trait KibitzerWakeDeliver: Send + Sync {
    fn deliver(
        &self,
        nudges: Vec<RecallNudge>,
        wake: u64,
        generation: u64,
    ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send>>;
}

/// The resident wake transitions (upstream `WakeTransitions`). Only `seed` is ported in this unit.
pub struct WakeTransitions {
    core: Arc<SidecarCore>,
    turns: Arc<TurnLifecycle>,
    admission: Arc<SidecarAdmission>,
    recovery: Arc<KibitzerRecovery>,
    /// The real child spawner: `spawn` creates the idle resident child.
    spawner: Arc<dyn KibitzerChildSpawner>,
    /// The retained host executor: every fire-and-forget transition is scheduled here, never on an
    /// ambient runtime.
    spawn: KibitzerWakeSpawn,
    /// The delivery seam `settle` awaits; the composition supplies it (see the trait).
    deliver: Arc<dyn KibitzerWakeDeliver>,
}

/// Builds the wake transitions for one sidecar (upstream `createWakeTransitions`).
///
/// `spawner` and `spawn` are the seams the core options do not carry (see the module docs); the
/// `deliver` seam is required by `settle`. The returned value is an `Arc`, because the settlement
/// driver a `seed` schedules on the retained executor must own the transitions to run `settle`.
pub fn create_wake_transitions(
    core: Arc<SidecarCore>,
    turns: Arc<TurnLifecycle>,
    admission: Arc<SidecarAdmission>,
    recovery: Arc<KibitzerRecovery>,
    spawner: Arc<dyn KibitzerChildSpawner>,
    spawn: KibitzerWakeSpawn,
    deliver: Arc<dyn KibitzerWakeDeliver>,
) -> Arc<WakeTransitions> {
    Arc::new(WakeTransitions { core, turns, admission, recovery, spawner, spawn, deliver })
}

impl WakeTransitions {
    /// `seed(fresh, maxItems)`: a fresh resident child whose first message is the carried and buffered
    /// events plus the fresh candidates. The lease is taken before the payload is cut, the deadline is
    /// armed before the child I/O it bounds, and the settlement is armed before the trigger.
    pub async fn seed(self: &Arc<Self>, fresh: &[RecallCandidate], max_items: usize) -> KibitzerOfferResult {
        let generation = self.core.record.lock().unwrap_or_else(PoisonError::into_inner).generations + 1;
        let admitted = match self.admission.admit().await.into_refusal() {
            Ok(admitted) => admitted,
            Err(refusal) => return self.admission.refused(refusal, generation, max_items, fresh.len()),
        };
        let Admission::Acquired { lease, waited_ms } = admitted else {
            // `into_refusal` yields `Ok` only for an acquired admission.
            unreachable!("an admitted wake always carries a lease")
        };
        // Cut the payload here: a hook captured while the child is starting opens the next batch
        // instead of being drained unread once the child exists. Carried payloads come first.
        let payload = {
            let batch = self.core.stream.lock().unwrap_or_else(PoisonError::into_inner).drain();
            let mut record = self.core.record.lock().unwrap_or_else(PoisonError::into_inner);
            let mut parts = std::mem::take(&mut record.carry);
            parts.push(payload_of(batch, fresh.to_vec()));
            merge(&parts)
        };
        let prompt = self.render_prompt(&payload, max_items);
        let turn = self.turns.new_turn(generation, max_items, payload.candidates.len());
        *turn.lease.lock().unwrap_or_else(PoisonError::into_inner) = Some(lease);
        turn.slot_wait_ms.store(waited_ms, Ordering::SeqCst);
        // The explicit minimal pending state: exactly one of {the expiry callback, the spawn
        // completion} claims it, so a spawn that succeeds AFTER the deadline fired can never install a
        // running child under an already-settled wake.
        let pending_claim = Arc::new(AtomicBool::new(false));
        let armed_epoch = Arc::new(AtomicU64::new(0));
        // Armed BEFORE the I/O it bounds: a `spawn` that never returns holds the machine-wide lease
        // and this session's chain, so nothing downstream could ever cut it off. The expiry releases
        // the lease and reports the wake directly, never queueing behind that I/O.
        self.turns.arm_deadline(
            &turn,
            Some(self.pending_start_expiry(&turn, Arc::clone(&pending_claim), Arc::clone(&armed_epoch))),
        );
        // The token `arm_deadline` minted for THIS arm: the expiry re-checks it before claiming, so a
        // re-arm (a spawn that succeeded) leaves the stale callback unable to claim.
        armed_epoch.store(turn.deadline_epoch.load(Ordering::SeqCst), Ordering::SeqCst);
        let input = KibitzerChildSpawnInput {
            session_id: self.core.session_id.clone(),
            generation,
            tools: KIBITZER_SIDECAR_TOOL_NAMES.iter().map(|name| (*name).to_string()).collect(),
            max_items,
        };
        let handle = match self.spawner.spawn(input).await {
            Ok(handle) => {
                // Atomically claim the pending start. A loss means the deadline already claimed and
                // settled this wake, so the late child begins no turn.
                if pending_claim.swap(true, Ordering::SeqCst) {
                    self.turns.abort_handle(Arc::clone(&handle), KibitzerWakeAbort::Deadline).await;
                    self.dispose_handle(&handle, generation);
                    return self.abandoned(payload);
                }
                handle
            }
            Err(error) => {
                // A loss means the deadline already claimed and reported this wake; its payload rides
                // the retry with no second report.
                if pending_claim.swap(true, Ordering::SeqCst) {
                    return self.abandoned(payload);
                }
                // No child read the envelope and nothing was offered: its events and candidates ride
                // the retry. The typed start failure is reported, then the sidecar backs off.
                self.turns.clear_deadline(&turn);
                self.core.record.lock().unwrap_or_else(PoisonError::into_inner).carry = vec![payload];
                self.admission.release_lease(&turn).await;
                let end = start_failure_end(&error.message, kibitzer_configuration_failure(&error));
                self.turns.report(&turn, &end, &[], None, None);
                self.recovery.enter_backoff();
                return KibitzerOfferResult::Buffered { reason: KibitzerBufferedReason::Backoff };
            }
        };
        // The child exists and its turn is about to start. The claim already proves the deadline did
        // not win, so the deadline's on_expire is superseded here.
        {
            let mut record = self.core.record.lock().unwrap_or_else(PoisonError::into_inner);
            record.generations = generation;
            record.pending_reseed = None;
        }
        self.turns.offer_paths(fresh, turn.wake);
        let unsubscribe = handle.subscribe_observations({
            let turns = Arc::clone(&self.turns);
            Arc::new(move |observation: KibitzerChildObservation| turns.observe(observation))
        });
        {
            let mut record = self.core.record.lock().unwrap_or_else(PoisonError::into_inner);
            record.child = Some(Child {
                handle: Arc::clone(&handle),
                generation,
                unsubscribes: Arc::new(Mutex::new(vec![unsubscribe])),
                usage_tokens: Arc::new(Mutex::new(None)),
                // `envelope.text.length` is UTF-16 units (JS `String.length`), not scalar values.
                chars_sent: Arc::new(Mutex::new(prompt.encode_utf16().count() as i64)),
            });
            record.active_turn = Some(Arc::clone(&turn));
            record.state = KibitzerSidecarState::TurnRunning;
        }
        turn.envelopes.lock().unwrap_or_else(PoisonError::into_inner).push(Envelope {
            text: prompt.clone(),
            payload: payload.clone(),
            steered: false,
            consumed: true,
        });
        self.turns.arm_deadline(&turn, None);
        // The settlement is armed BEFORE the trigger, and the arm is SYNCHRONOUS (see `arm_settlement`).
        self.arm_settlement(Arc::clone(&handle), Arc::clone(&turn));
        if let Err(error) = handle.follow_up(&prompt).await {
            // The turn is begun and its failure is delivered through the armed settlement, exactly as
            // the child's own `fail_settle` reports it - never a second report from here.
            self.core.warn(
                "omo-senpi kibitzer sidecar seed trigger failed",
                Some(json!({ "wake": turn.wake, "error": error })),
            );
        }
        KibitzerOfferResult::Seeded { wake: turn.wake }
    }

    /// `followUp(current, fresh, maxItems)`: the IDLE resident child is revived with the carried and
    /// buffered events. The resident generation is kept, a fresh lease and accepted/budget slots are
    /// taken, the carried payload precedes the fresh batch, and the turn is installed BEFORE the
    /// trigger so the child's own observations and the deadline apply during the native turn.
    ///
    /// Runs INSIDE the existing per-session chain (its callers wrapped it in `SidecarCore::serialized`)
    /// and never calls `serialized` itself. The applied native `follow_up` awaits the whole turn, so the
    /// turn must be active before it; the CLI trigger-scheduling seam (followup receipt) lets the
    /// trigger return once the turn starts, which is what makes the serialized normal abort reachable.
    pub async fn follow_up(
        self: &Arc<Self>,
        current: Arc<dyn KibitzerChild>,
        fresh: &[RecallCandidate],
        max_items: usize,
    ) -> KibitzerOfferResult {
        // The resident generation and the report context come from the SAME child the turn revives; the
        // snapshot shares every `Arc` field, so it stays valid after the record lock is dropped.
        let child_snapshot = self.core.record.lock().unwrap_or_else(PoisonError::into_inner).child.clone();
        let generation = child_snapshot.as_ref().map(|child| child.generation).unwrap_or(0);
        let admitted = match self.admission.admit().await.into_refusal() {
            Ok(admitted) => admitted,
            Err(refusal) => return self.admission.refused(refusal, generation, max_items, fresh.len()),
        };
        let Admission::Acquired { lease, waited_ms } = admitted else {
            // `into_refusal` yields `Ok` only for an acquired admission.
            unreachable!("an admitted wake always carries a lease")
        };
        // Cut the payload here, as seed does: carried payloads first, then the drained batch.
        let payload = {
            let batch = self.core.stream.lock().unwrap_or_else(PoisonError::into_inner).drain();
            let mut record = self.core.record.lock().unwrap_or_else(PoisonError::into_inner);
            let mut parts = std::mem::take(&mut record.carry);
            parts.push(payload_of(batch, fresh.to_vec()));
            merge(&parts)
        };
        let text = {
            let adapter = envelope_input(&self.core, payload.clone(), max_items, false);
            render_kibitzer_wake_prompt(&adapter.input())
        };
        let turn = self.turns.new_turn(generation, max_items, payload.candidates.len());
        *turn.lease.lock().unwrap_or_else(PoisonError::into_inner) = Some(lease);
        turn.slot_wait_ms.store(waited_ms, Ordering::SeqCst);
        self.turns.offer_paths(fresh, turn.wake);
        // Arm the child's own turn end BEFORE the trigger. The settlement transition is serialized
        // behind this one, so it runs only after this transition returns.
        self.arm_settlement(Arc::clone(&current), Arc::clone(&turn));
        // Install the turn BEFORE the trigger: the applied native trigger awaits the whole turn, so the
        // turn must already be active for the child's observations to be seen and the deadline to apply.
        {
            let mut record = self.core.record.lock().unwrap_or_else(PoisonError::into_inner);
            record.active_turn = Some(Arc::clone(&turn));
            record.state = KibitzerSidecarState::TurnRunning;
        }
        if let Some(child) = child_snapshot.as_ref() {
            // `envelope.text.length` is UTF-16 units (JS `String.length`); the counter is `Arc`-shared.
            *child.chars_sent.lock().unwrap_or_else(PoisonError::into_inner) += text.encode_utf16().count() as i64;
        }
        turn.envelopes.lock().unwrap_or_else(PoisonError::into_inner).push(Envelope {
            text: text.clone(),
            payload: payload.clone(),
            steered: false,
            consumed: true,
        });
        self.turns.arm_deadline(&turn, None);
        match current.follow_up(&text).await {
            Err(error) => {
                // The revival failed: unwind the installed turn, carry the payload, report the child
                // failure OUTSIDE the record lock with the real child context, dispose the child and
                // back off - matching upstream's failed followUp.
                self.turns.clear_deadline(&turn);
                {
                    let mut record = self.core.record.lock().unwrap_or_else(PoisonError::into_inner);
                    record.active_turn = None;
                    record.carry = vec![payload];
                }
                self.admission.release_lease(&turn).await;
                let end = KibitzerWakeEnd::Failed {
                    cause: KibitzerWakeFailureCause::ChildFailed,
                    reason: Some(error),
                    configuration: None,
                };
                self.turns.report(&turn, &end, &[], child_snapshot.as_ref(), None);
                self.recovery.dispose_child();
                self.recovery.enter_backoff();
                KibitzerOfferResult::Buffered { reason: KibitzerBufferedReason::Backoff }
            }
            Ok(()) => KibitzerOfferResult::FollowedUp { wake: turn.wake },
        }
    }

    /// `steer(current, turn, fresh, maxItems)`: the running turn absorbs the batch. A steer JOINS the
    /// turn under its existing machine-wide lease, so no new wake is minted and the accepted list and
    /// the tool budget are NEVER reset. The fresh candidates are offered, the batch's candidates join
    /// the turn's candidate count, and the steered envelope is appended UNCONSUMED so a steer that
    /// fails is replayed by settlement. The bounded deadline is re-armed (the quiet period restarts)
    /// BEFORE the steer it bounds; a steer failure only warns and leaves the envelope unconsumed.
    pub async fn steer(
        self: &Arc<Self>,
        current: &Child,
        turn: &Arc<Turn>,
        fresh: &[RecallCandidate],
        max_items: usize,
    ) -> KibitzerOfferResult {
        // The batch joins the turn: only the buffered events plus the fresh candidates, never the carry.
        let payload = {
            let batch = self.core.stream.lock().unwrap_or_else(PoisonError::into_inner).drain();
            payload_of(batch, fresh.to_vec())
        };
        let adapter = envelope_input(&self.core, payload.clone(), max_items, false);
        let text = render_kibitzer_wake_prompt(&adapter.input());
        self.turns.offer_paths(fresh, turn.wake);
        turn.candidate_count.fetch_add(fresh.len(), Ordering::SeqCst);
        turn.envelopes.lock().unwrap_or_else(PoisonError::into_inner).push(Envelope {
            text: text.clone(),
            payload,
            steered: true,
            consumed: false,
        });
        // `envelope.text.length` is UTF-16 units (JS `String.length`), not scalar values.
        *current.chars_sent.lock().unwrap_or_else(PoisonError::into_inner) += text.encode_utf16().count() as i64;
        // Re-arm the bounded deadline BEFORE the steer it bounds: the quiet period restarts.
        self.turns.arm_deadline(turn, None);
        if let Err(error) = current.handle.steer(&text).await {
            // Left unconsumed on purpose: settlement replays it through the followUp.
            self.core.warn(
                "omo-senpi kibitzer sidecar steer failed",
                Some(json!({ "wake": turn.wake, "error": error })),
            );
        }
        KibitzerOfferResult::Steered { wake: turn.wake }
    }

    /// The first message: a seed envelope, or - when a restart is pending - the reseed envelope
    /// concatenated with the wake envelope, both rendered over the SAME cut payload.
    fn render_prompt(&self, payload: &Payload, max_items: usize) -> String {
        let pending = self.core.record.lock().unwrap_or_else(PoisonError::into_inner).pending_reseed.clone();
        match pending {
            None => {
                let adapter = envelope_input(&self.core, payload.clone(), max_items, true);
                render_kibitzer_seed_prompt(&adapter.input())
            }
            Some(reseed) => {
                let adapter = envelope_input(&self.core, payload.clone(), max_items, false);
                let wake = render_kibitzer_wake_prompt(&adapter.input());
                let input = KibitzerReseedInput {
                    session_id: &reseed.session_id,
                    max_items,
                    last_cursor: reseed.last_cursor,
                    task_summary: &reseed.task_summary,
                    rejected_paths: &reseed.rejected_paths,
                    delivered_paths: &reseed.delivered_paths,
                    tool_budget: Some(self.core.tool_budget),
                    caps: reseed.caps,
                    max_chars: reseed.max_chars,
                };
                format!("{}{}", render_kibitzer_reseed_prompt(&input), wake)
            }
        }
    }

    /// The pending-start deadline callback: it schedules [`abandon_pending_start`] on the retained
    /// executor, so the release and the report never queue behind the I/O this deadline bounds.
    fn pending_start_expiry(
        &self,
        turn: &Arc<Turn>,
        pending_claim: Arc<AtomicBool>,
        armed_epoch: Arc<AtomicU64>,
    ) -> Box<dyn FnOnce() + Send> {
        let turns = Arc::clone(&self.turns);
        let admission = Arc::clone(&self.admission);
        let spawn = Arc::clone(&self.spawn);
        let turn = Arc::clone(turn);
        Box::new(move || {
            spawn(Box::pin(async move {
                abandon_pending_start(turns, admission, turn, pending_claim, armed_epoch).await;
            }));
        })
    }

    /// Arms the child's own turn end BEFORE the trigger and hands the outcome to [`Self::settle_transition`]
    /// as the next serialized step.
    ///
    /// `KibitzerChild::settle` installs its receiver when it is CALLED - the applied CLI child stores
    /// the oneshot sender at call time - so the arm happens HERE, synchronously, and NOT inside the
    /// spawned driver: the retained executor may not poll the driver before `follow_up`, so scheduling
    /// alone would prove nothing. The driver only AWAITS the already-armed future.
    ///
    /// The armed future must be movable into the `'static` driver, so `KibitzerChild::settle` must
    /// return `Pin<Box<dyn Future<Output = JudgeSettle> + Send>>` (no `'_`); see the receipt. The driver
    /// owns the `Arc<Self>` so it can run the sibling `settle` transition.
    fn arm_settlement(self: &Arc<Self>, handle: Arc<dyn KibitzerChild>, turn: Arc<Turn>) {
        let settle = handle.settle();
        let wake = Arc::clone(self);
        let core = Arc::clone(&self.core);
        (self.spawn)(Box::pin(async move {
            let outcome = settle.await;
            core.serialized(move || {
                let wake = Arc::clone(&wake);
                async move { wake.settle_transition(turn, outcome).await }
            })
            .await;
        }));
    }

    /// `settle(turn, outcome)`: the turn is over. The lease goes back FIRST. Only the ACTIVE turn of a
    /// live child is settled further; a superseded settlement returns early but STILL resolves its own
    /// idle signal, because upstream `turn.settled` is the settle transition's own promise (which
    /// resolves however the transition returns). Nudges are re-validated and delivered (a delivery
    /// failure only warns); accepted paths become delivered and surfaced and charge the cooldown; unread
    /// envelopes ride the carry; a failed wake forgets the offered paths it could not deliver; then the
    /// next state is chosen exactly as upstream. The idle signal resolves only after the whole
    /// transition completes, matching upstream where `whenIdle` awaits the settle promise itself.
    ///
    /// Runs inside the per-session chain (its caller wrapped it in `SidecarCore::serialized`), so the
    /// deliver `.await` never holds a record/stream lock.
    pub async fn settle_transition(self: &Arc<Self>, turn: Arc<Turn>, outcome: JudgeSettle) {
        // The provider turn is over whatever happens next: the machine slot goes back first.
        self.admission.release_lease(&turn).await;
        let active = {
            let record = self.core.record.lock().unwrap_or_else(PoisonError::into_inner);
            record.active_turn.as_ref().is_some_and(|current| Arc::ptr_eq(current, &turn))
        };
        let handle = {
            let record = self.core.record.lock().unwrap_or_else(PoisonError::into_inner);
            record.child.as_ref().map(|child| Arc::clone(&child.handle))
        };
        // A superseded settlement (not the active turn, or no live child) changes nothing else - but
        // its own idle signal still resolves, so a waiter on this turn can never hang. `settle` is
        // idempotent, so a later real settlement of the same turn is unaffected.
        let (Some(handle), true) = (handle, active) else {
            turn.settled.settle();
            return;
        };
        self.turns.clear_deadline(&turn);
        let abort = *turn.abort.lock().unwrap_or_else(PoisonError::into_inner);
        let accepted = turn.accepted.lock().unwrap_or_else(PoisonError::into_inner).clone();
        let end = classify_wake_end(&outcome, abort, &accepted);
        let nudges: Vec<RecallNudge> = if matches!(end, KibitzerWakeEnd::Cancelled) {
            Vec::new()
        } else {
            let record = self.core.record.lock().unwrap_or_else(PoisonError::into_inner);
            match record.child.as_ref() {
                Some(current) => self.turns.validated(&turn, current),
                None => Vec::new(),
            }
        };
        if !nudges.is_empty() {
            if let Err(error) = self.deliver.deliver(nudges.clone(), turn.wake, turn.generation).await {
                self.core.warn(
                    "omo-senpi kibitzer sidecar delivery failed",
                    Some(json!({ "wake": turn.wake, "error": error })),
                );
            }
            {
                let mut record = self.core.record.lock().unwrap_or_else(PoisonError::into_inner);
                for nudge in &nudges {
                    record.delivered.insert(nudge.path.clone());
                }
            }
            {
                let mut surfaced = self.core.resources.surfaced.lock().unwrap_or_else(PoisonError::into_inner);
                for nudge in &nudges {
                    surfaced.insert(nudge.path.clone());
                }
            }
            self.core.cooldown.lock().unwrap_or_else(PoisonError::into_inner).charge();
        }
        // A dead child judged nothing it was sent; a live one may have missed steers that landed late.
        let failed = matches!(end, KibitzerWakeEnd::Failed { .. });
        let unread: Vec<Payload> = {
            let envelopes = turn.envelopes.lock().unwrap_or_else(PoisonError::into_inner);
            envelopes
                .iter()
                .filter(|envelope| failed || (envelope.steered && !envelope.consumed))
                .map(|envelope| envelope.payload.clone())
                .collect()
        };
        if failed {
            let paths: Vec<String> = {
                let envelopes = turn.envelopes.lock().unwrap_or_else(PoisonError::into_inner);
                envelopes
                    .iter()
                    .flat_map(|envelope| envelope.payload.candidates.iter().map(|candidate| candidate.path.clone()))
                    .collect()
            };
            let mut record = self.core.record.lock().unwrap_or_else(PoisonError::into_inner);
            let mut offered = self.core.resources.offered.lock().unwrap_or_else(PoisonError::into_inner);
            for path in paths {
                if record.delivered.contains(&path) {
                    continue;
                }
                offered.remove(&path);
                record.offered_at_wake.remove(&path);
            }
        }
        {
            let mut record = self.core.record.lock().unwrap_or_else(PoisonError::into_inner);
            record.carry.extend(unread);
            record.active_turn = None;
        }
        // Clone the child snapshot OUT of the record, drop the guard, then report: the `on_wake`
        // callback may reenter the sidecar (`core.state()` locks the record), so no record lock may be
        // held across `report`. The snapshot shares every `Arc` field with the live child.
        let child_snapshot = self.core.record.lock().unwrap_or_else(PoisonError::into_inner).child.clone();
        self.turns.report(&turn, &end, &nudges, child_snapshot.as_ref(), None);
        match &end {
            KibitzerWakeEnd::Cancelled => {
                self.recovery.dispose_child();
                self.set_state(KibitzerSidecarState::Disposed);
            }
            KibitzerWakeEnd::Failed { .. } => {
                self.recovery.dispose_child();
                self.recovery.enter_backoff();
            }
            KibitzerWakeEnd::Completed | KibitzerWakeEnd::ToolBudgetExceeded | KibitzerWakeEnd::Deadline => {
                self.core.record.lock().unwrap_or_else(PoisonError::into_inner).consecutive_failures = 0;
                let estimate = {
                    let record = self.core.record.lock().unwrap_or_else(PoisonError::into_inner);
                    record.child.as_ref().map(|child| self.turns.context_estimate(child)).unwrap_or(0)
                };
                if estimate >= self.core.reseed_at_tokens {
                    self.recovery.prepare_reseed(turn.wake);
                    self.recovery.dispose_child();
                    self.set_state(KibitzerSidecarState::Reseeding);
                } else {
                    let carry_len = self.core.record.lock().unwrap_or_else(PoisonError::into_inner).carry.len();
                    if carry_len > 0 {
                        // The later followUp unit's transition; invoked exactly as upstream's
                        // `await followUp(current, [], turn.maxItems)`. It is called from INSIDE the
                        // chain, so it must not call `serialized` itself (upstream's followUp does not).
                        let _ = self.follow_up(handle, &[], turn.max_items).await;
                    } else {
                        self.set_state(KibitzerSidecarState::Idle);
                    }
                }
            }
        }
        // The idle signal resolves only once the WHOLE settle transition has completed (including the
        // follow-up it may have started), matching upstream, where `whenIdle` awaits the settle promise
        // itself. `settle` is idempotent.
        turn.settled.settle();
    }

    /// Sets the sidecar state under a short, non-await record lock.
    fn set_state(&self, state: KibitzerSidecarState) {
        self.core.record.lock().unwrap_or_else(PoisonError::into_inner).state = state;
    }

    /// What the transition returns once its child I/O finally lands behind an abandoned deadline: no
    /// child read the envelope, so its events and candidates ride the retry, as a start failure's do.
    fn abandoned(&self, payload: Payload) -> KibitzerOfferResult {
        self.core.record.lock().unwrap_or_else(PoisonError::into_inner).carry = vec![payload];
        self.recovery.enter_backoff();
        KibitzerOfferResult::Buffered { reason: KibitzerBufferedReason::Backoff }
    }

    /// Disposes a child that arrived after its wake was abandoned. The async native disposal runs on
    /// the retained executor; a failure is warned, never dropped and never blocking the reactor.
    fn dispose_handle(&self, handle: &Arc<dyn KibitzerChild>, generation: u64) {
        let handle = Arc::clone(handle);
        let warn = self.core.options.warn.clone();
        (self.spawn)(Box::pin(async move {
            if let Err(error) = handle.dispose().await {
                if let Some(warn) = warn {
                    warn(&format!(
                        "omo-senpi kibitzer sidecar dispose failed {}",
                        json!({ "generation": generation, "error": error })
                    ));
                }
            }
        }));
    }
}

/// The pending-start deadline: the transition holding the chain is the very I/O this bounds, so the
/// abort cannot queue behind it. Before claiming, the callback re-checks the deadline epoch it was
/// armed under (a re-arm by a successful spawn has advanced it, leaving this callback stale), then
/// atomically claims the pending start. Only a claimed expiry hands the lease back and reports.
async fn abandon_pending_start(
    turns: Arc<TurnLifecycle>,
    admission: Arc<SidecarAdmission>,
    turn: Arc<Turn>,
    pending_claim: Arc<AtomicBool>,
    armed_epoch: Arc<AtomicU64>,
) {
    let owned = armed_epoch.load(Ordering::SeqCst);
    if owned != 0 && turn.deadline_epoch.load(Ordering::SeqCst) != owned {
        return;
    }
    if pending_claim.swap(true, Ordering::SeqCst) {
        return;
    }
    *turn.abort.lock().unwrap_or_else(PoisonError::into_inner) = Some(KibitzerWakeAbort::Deadline);
    turns.clear_deadline(&turn);
    admission.release_lease(&turn).await;
    turns.report(&turn, &KibitzerWakeEnd::Deadline, &[], None, None);
}
