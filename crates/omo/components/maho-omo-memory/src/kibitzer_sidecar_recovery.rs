//! How the sidecar lets go of a child and prepares the next one (latest `kibitzer/sidecar-recovery.ts`).
//!
//! The reseed state a replacement child inherits at the context budget, the jittered backoff after a
//! failure, and disposal. Every method is a TRANSITION BODY: the composition root invokes it from
//! inside a `SidecarCore::serialized` closure (its `seed` / `settle` / `shutdown`), so this module
//! never calls `serialized` itself and never holds a `std::sync::MutexGuard` across an `.await` -
//! each record lock covers a short, non-await section and is released before any callback or
//! executor hand-off.
//!
//! # Scope
//!
//! This module ports `sidecar-recovery.ts` and nothing else: `prepareReseed`, `enterBackoff`,
//! `clearBackoff` and `disposeChild`. Carry replay, admission, wake triggering and settlement belong
//! to their own units. Every field it advances lives on the applied core (`offered_at_wake`,
//! `delivered`, `pending_reseed`, `task_summary`, `state`, `consecutive_failures`, `backoff_timer`,
//! `child`) and the live `offered` set it narrows lives on the shared `KibitzerSessionResources`; the
//! delay comes from the existing `kibitzer_backoff` helper. No parallel state is created.
//!
//! # Timer identity (why the backoff callback is chain-serialized and token-guarded)
//!
//! `enterBackoff` arms a timer whose callback returns the sidecar to `Idle` - but only for the CURRENT
//! backoff, and never before `enterBackoff` has stored the timer handle. Two native hazards do not
//! exist upstream:
//!
//! * a `KibitzerSidecarTimers::set` implementation is free to invoke the callback before it returns
//!   the handle, which would race the handle store; and
//! * a timer replaced by a later `enterBackoff` may already have SCHEDULED its callback, and aborting
//!   the handle does not unschedule a future already handed to the executor, so a stale callback
//!   could clear the newer backoff.
//!
//! Both are closed WITHOUT a core or trait change. The callback never mutates the record inline: it
//! schedules a transition on the host [`KibitzerWakeSpawn`] that first awaits
//! [`SidecarCore::serialized`], so it cannot run until the `enterBackoff` transition that armed it
//! (which holds the chain) has stored the handle and released the chain - a synchronous `set` is
//! therefore safe. Each arm also mints a fresh token from the owned [`AtomicU64`] generation, and the
//! callback clears/restores state only while its own token is still current - checked INSIDE the
//! serialized transition - so a stale callback for a replaced or cleared timer can never clear a newer
//! backoff. The delay and the ambient runtime are never relied on.
//!
//! # Deviations (each an N/A, because the Rust crate has no direct counterpart)
//!
//! * Upstream `createRecovery(core)` returns four closures over `core`; the port returns a
//!   [`KibitzerRecovery`] holding `core` plus the host's explicit executor ([`KibitzerWakeSpawn`]).
//!   A native `KibitzerChild::dispose` is an async future that must be driven by an executor (JS
//!   `handle.dispose()` is fire-and-forget, so upstream needs none), and the core record owns no
//!   executor, so the seam is passed in. The composition root already holds the same `spawn` it gives
//!   the wake runner; no core field is added and no parallel state is created.
//! * `offeredAtWake` is the core's `BTreeMap` and `delivered` its `BTreeSet`, so the rejected and
//!   delivered path lists are ordered by path rather than by JS `Map` / `Set` insertion order. N/A:
//!   the core owner chose the ordered containers; the port does not re-order them.
//! * `core.stream.lastCursor() ?? 0` is `last_cursor().unwrap_or(0)`: the applied event producer's
//!   `last_cursor()` returns `Option<usize>` (absent until the stream has seen a cursor), and
//!   `describe(error)` is the identity because the child's `dispose` already yields a `String`.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, PoisonError};

use serde_json::json;

use crate::kibitzer_backoff::{KIBITZER_BACKOFF_MAX_MS, KIBITZER_BACKOFF_MIN_MS, backoff_delay_ms};
use crate::kibitzer_contract::{KIBITZER_REJECTED_AFTER_WAKES, KibitzerSidecarState, KibitzerWakeSpawn};
use crate::kibitzer_prompt_blocks::KIBITZER_FIELD_CAPS;
use crate::kibitzer_sidecar_core::{PendingReseed, SidecarCore};

/// The recovery surface (upstream `SidecarRecovery`): reseed preparation, jittered backoff and child
/// disposal. Built by [`create_recovery`]; every method is called from inside a serialized transition
/// and never takes the chain itself. `spawn` drives the async native disposal and the
/// chain-serialized backoff callback; `backoff_generation` identifies the currently armed timer.
pub struct KibitzerRecovery {
    core: Arc<SidecarCore>,
    /// The host executor the async native `dispose` and the chain-serialized backoff callback run on;
    /// never the ambient runtime.
    spawn: KibitzerWakeSpawn,
    /// The identity of the CURRENTLY armed backoff timer. `enter_backoff` mints a fresh value and
    /// `clear_backoff` invalidates it; a timer callback acts only while its captured value is still
    /// current, so a callback for a replaced or cleared timer can never clear a newer backoff.
    backoff_generation: Arc<AtomicU64>,
}

/// Builds the recovery for one sidecar (upstream `createRecovery(core)`), plus the explicit executor
/// the native async disposal and the backoff callback need (see the module notes).
pub fn create_recovery(core: Arc<SidecarCore>, spawn: KibitzerWakeSpawn) -> KibitzerRecovery {
    KibitzerRecovery { core, spawn, backoff_generation: Arc::new(AtomicU64::new(0)) }
}

impl KibitzerRecovery {
    /// `prepareReseed(wake)`: the replacement child inherits only what a restart would otherwise
    /// lose - paths declined through `KIBITZER_REJECTED_AFTER_WAKES` wake opportunities, paths already
    /// delivered, the task line and the last cursor. Every other offered path is forgotten so it may
    /// wake the new child again. `surfaced` and delivery state are left untouched.
    pub fn prepare_reseed(&self, wake: u64) {
        let (rejected, delivered_paths, task_summary) = {
            let record = self.core.record.lock().unwrap_or_else(PoisonError::into_inner);
            let rejected: Vec<String> = record
                .offered_at_wake
                .iter()
                .filter(|&(path, at)| {
                    !record.delivered.contains(path.as_str())
                        && (wake as i64 - *at as i64 + 1) >= KIBITZER_REJECTED_AFTER_WAKES as i64
                })
                .map(|(path, _)| path.as_str().to_string())
                .collect();
            let delivered_paths: Vec<String> = record.delivered.iter().cloned().collect();
            (rejected, delivered_paths, record.task_summary.clone().unwrap_or_default())
        };
        let last_cursor = self.core.stream.lock().unwrap_or_else(PoisonError::into_inner).last_cursor().unwrap_or(0);
        {
            let mut record = self.core.record.lock().unwrap_or_else(PoisonError::into_inner);
            record.pending_reseed = Some(PendingReseed {
                session_id: self.core.session_id.clone(),
                last_cursor,
                task_summary,
                rejected_paths: rejected.clone(),
                delivered_paths: delivered_paths.clone(),
                caps: KIBITZER_FIELD_CAPS,
                max_chars: None,
            });
        }
        // Offered membership becomes rejected union delivered; every other offered path is dropped.
        let retained: BTreeSet<String> = rejected.iter().chain(delivered_paths.iter()).cloned().collect();
        {
            let mut offered = self.core.resources.offered.lock().unwrap_or_else(PoisonError::into_inner);
            offered.clear();
            offered.extend(retained.iter().cloned());
        }
        // offered_at_wake keeps only entries whose path is still offered.
        {
            let mut record = self.core.record.lock().unwrap_or_else(PoisonError::into_inner);
            record.offered_at_wake.retain(|path, _| retained.contains(path));
        }
    }

    /// `enterBackoff()`: exponential backoff with jitter. The delay is computed from the OLD failure
    /// count and the injected random, the count is incremented once, the state becomes `Backoff` and
    /// the prior timer (if any) is replaced. The timer returns the sidecar to `Idle` only while the
    /// state is still `Backoff` - a shutdown that reached `Disposed` first is never resurrected, and
    /// nothing is recreated. The callback is chain-serialized and token-guarded, so a synchronous
    /// `set` cannot mutate before the handle is stored and a replaced timer's stale callback cannot
    /// clear the new backoff (see the module note).
    pub fn enter_backoff(&self) {
        let delay = {
            let mut record = self.core.record.lock().unwrap_or_else(PoisonError::into_inner);
            let delay = backoff_delay_ms(
                record.consecutive_failures as usize,
                self.core.random.as_ref(),
                KIBITZER_BACKOFF_MIN_MS,
                KIBITZER_BACKOFF_MAX_MS,
            );
            record.consecutive_failures += 1;
            record.state = KibitzerSidecarState::Backoff;
            delay
        };
        self.clear_backoff();
        let token = self.backoff_generation.fetch_add(1, Ordering::SeqCst) + 1;
        let core = Arc::clone(&self.core);
        let spawn = Arc::clone(&self.spawn);
        let generation = Arc::clone(&self.backoff_generation);
        let handle = self.core.timers.set(
            Box::new(move || {
                // Schedule, never mutate inline: the transition waits for the chain, which the arming
                // `enter_backoff` transition still holds until the handle below is stored.
                (spawn)(Box::pin(async move {
                    let task_core = Arc::clone(&core);
                    core.serialized(move || async move {
                        // Inside the chain: a replaced or cleared timer's token no longer matches.
                        if generation.load(Ordering::SeqCst) != token {
                            return;
                        }
                        let mut record = task_core.record.lock().unwrap_or_else(PoisonError::into_inner);
                        record.backoff_timer = None;
                        if record.state == KibitzerSidecarState::Backoff {
                            record.state = KibitzerSidecarState::Idle;
                        }
                    })
                    .await;
                }));
            }),
            delay,
        );
        {
            let mut record = self.core.record.lock().unwrap_or_else(PoisonError::into_inner);
            record.backoff_timer = Some(handle);
        }
    }

    /// `clearBackoff()`: aborts a pending backoff timer, if any. The current identity is invalidated
    /// FIRST, so a callback already handed to the executor stops matching and cannot clear a later
    /// backoff even though `clear` cannot unschedule it.
    pub fn clear_backoff(&self) {
        self.backoff_generation.fetch_add(1, Ordering::SeqCst);
        let handle = {
            let mut record = self.core.record.lock().unwrap_or_else(PoisonError::into_inner);
            record.backoff_timer.take()
        };
        if let Some(handle) = handle {
            self.core.timers.clear(handle);
        }
    }

    /// `disposeChild()`: unsubscribes and disposes the current child, if any. The child is removed
    /// FIRST, so a second call sees none; the unsubscribe closures run before the async native
    /// disposal is handed to the executor, so a torn-down child stops reporting. A dispose failure is
    /// reported through the warn sink, never thrown, never dropped, and never blocks the reactor.
    pub fn dispose_child(&self) {
        let current = {
            let mut record = self.core.record.lock().unwrap_or_else(PoisonError::into_inner);
            record.child.take()
        };
        let Some(current) = current else { return };
        let unsubscribes = std::mem::take(&mut *current.unsubscribes.lock().unwrap_or_else(PoisonError::into_inner));
        for unsubscribe in unsubscribes {
            unsubscribe();
        }
        let handle = Arc::clone(&current.handle);
        let generation = current.generation;
        let warn = self.core.options.warn.clone();
        (self.spawn)(Box::pin(async move {
            if let Err(error) = handle.dispose().await
                && let Some(warn) = warn
            {
                warn(&format!("omo-senpi kibitzer sidecar dispose failed {}", json!({ "generation": generation, "error": error })));
            }
        }));
    }
}
