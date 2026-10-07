//! Lazy host adapter from the memory-owned passive-entry port to the shared idle-injection queue.
//!
//! The queue lives on the retained `OmoRuntime`, filled only when the OMO composition registers -
//! AFTER this adapter is built - so the cell is read per call and no second queue is created. The
//! memory trait's `enqueue` returns `bool` (the refusal contract), forwarded verbatim: a refused
//! entry stays owned by the caller, never silently dropped.

use std::sync::{Arc, Mutex, PoisonError};

use maho_omo::OmoRuntime;
use maho_omo_memory::kibitzer_delivery::{KibitzerCoordinatorEntry, KibitzerIdleCoordinator};

pub struct CliKibitzerCoordinator {
    runtime: Arc<Mutex<Option<OmoRuntime>>>,
}

/// `omo_cell` is the SAME cell `memory_for_parent` fills at register time.
pub fn create_kibitzer_coordinator(runtime: Arc<Mutex<Option<OmoRuntime>>>) -> Arc<dyn KibitzerIdleCoordinator> {
    Arc::new(CliKibitzerCoordinator { runtime })
}

impl KibitzerIdleCoordinator for CliKibitzerCoordinator {
    /// `false` when the queue is not retained yet or refuses the entry (retired): the caller keeps
    /// ownership and must fail it durably - never a silent success.
    fn enqueue(&self, entry: KibitzerCoordinatorEntry) -> bool {
        let Some(runtime) = self.runtime.lock().unwrap_or_else(PoisonError::into_inner).clone() else { return false; };
        // Upstream kibitzer rank 6; `passive` rides a non-passive flush and never causes one.
        maho_ext_api::IdleInjectionCoordinator::enqueue(&runtime.coordinator(), maho_ext_api::IdleInjection {
            key: entry.key,
            source: maho_ext_api::IdleInjectionSource::Kibitzer,
            custom_type: Some(entry.custom_type),
            content: entry.content,
            display: Some(false),
            details: Some(entry.details),
            passive: Some(entry.passive),
            on_flushed: Some(entry.on_flushed),
            on_delivery_failed: None,
        })
    }
    fn remove(&self, key: &str) {
        if let Some(runtime) = self.runtime.lock().unwrap_or_else(PoisonError::into_inner).clone() {
            maho_ext_api::IdleInjectionCoordinator::remove(&runtime.coordinator(), key);
        }
    }
}
