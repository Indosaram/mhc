//! Idle-edge injection contract shared by the omo host composition and its components.
//!
//! `maho_ext_api::ExtensionContext::idle_coordinator` is an opaque `Any` slot; [`into_slot`] and
//! [`from_slot`] convert between that slot and an `Arc<dyn IdleInjectionCoordinator>`.
use std::any::Any;
use std::sync::Arc;

use maho_ext_api::JsonValue;

/// One queued idle-edge injection (upstream `IdleInjection`, idle-injection-coordinator.ts).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// Upstream source set. The PINNED `idle-injection-coordinator.ts` (77f3067f1) carries the first six;
/// `WorkpoolAggregate` and `Kibitzer` are LATEST additions (audit checkout 20261006), selected for the
/// recall production integration and recorded as latest, NOT pinned parity.
pub enum IdleInjectionSource {
    TaskCompletion,
    /// Latest (`workpool-aggregate`); ranked with `TaskCompletion`.
    WorkpoolAggregate,
    TeamMessage,
    TeamLiveness,
    BoulderContinuation,
    UlwContinuation,
    DagRun,
    /// Latest (`kibitzer`); ranked last, always queued `passive`.
    Kibitzer,
}
/// `onFlushed` callback: runs once per injection after a successful delivery.
pub type IdleInjectionCallback = Arc<dyn Fn() + Send + Sync>;
/// `onDeliveryFailed` callback: receives the delivery error message.
pub type IdleInjectionFailureCallback = Arc<dyn Fn(&str) + Send + Sync>;
#[derive(Clone)]
pub struct IdleInjection {
    pub key: String,
    pub source: IdleInjectionSource,
    pub custom_type: Option<String>,
    pub content: String,
    pub display: Option<bool>,
    pub details: Option<JsonValue>,
    /// LATEST addition (absent from the pinned TS): `passive?: boolean` rides a flush carrying at
    /// least one non-passive entry and never causes a flush by itself. `None` = non-passive.
    pub passive: Option<bool>,
    pub on_flushed: Option<IdleInjectionCallback>,
    pub on_delivery_failed: Option<IdleInjectionFailureCallback>,
}
/// The single idle-edge injection queue. The concrete implementation lives in the composition
/// (`crates/omo/maho-omo`, todo 47); this crate defines only the contract so components can use
/// it without depending on that crate, and `ExtensionContext` carries it as an opaque slot.
/// `None` means "no coordinator" and every consumer falls back to a direct send, matching
/// upstream's optional `ComponentContext.idleCoordinator`.
pub trait IdleInjectionCoordinator: Send + Sync {
    /// Accept an injection into the batch window. Returns `false` when the coordinator is retired:
    /// the queue is gone, the injection is NOT queued and gets no receipt, so the caller still owns
    /// the notification and must fail it durably (upstream `enqueue(...): boolean`).
    fn enqueue(&self, injection: IdleInjection) -> bool;
    /// Retire the coordinator on session shutdown (upstream `retire()`): refuse further enqueues and
    /// fail every still-queued injection through `on_delivery_failed`. A coordinator that is never
    /// retired keeps this default no-op and always accepts.
    fn retire(&self) {}
    fn schedule_flush(&self);
    fn flush_soon(&self);
    fn flush_on_idle(&self) -> usize;
    fn pending_count(&self) -> usize;
    fn remove(&self, key: &str) -> bool;
}

/// Wrap a coordinator for `ExtensionContext::idle_coordinator`.
pub fn into_slot(coordinator: Arc<dyn IdleInjectionCoordinator>) -> Arc<dyn Any + Send + Sync> {
    Arc::new(coordinator)
}

/// Read the coordinator back out of `ExtensionContext::idle_coordinator`. `None` when the slot is
/// empty or holds a value that was not stored through [`into_slot`].
pub fn from_slot(slot: &Option<Arc<dyn Any + Send + Sync>>) -> Option<Arc<dyn IdleInjectionCoordinator>> {
    slot.as_ref()?.downcast_ref::<Arc<dyn IdleInjectionCoordinator>>().cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Default)]
    struct Counter(AtomicUsize);
    impl IdleInjectionCoordinator for Counter {
        fn enqueue(&self, _injection: IdleInjection) -> bool {
            self.0.fetch_add(1, Ordering::SeqCst);
            true
        }
        fn schedule_flush(&self) {}
        fn flush_soon(&self) {}
        fn flush_on_idle(&self) -> usize {
            0
        }
        fn pending_count(&self) -> usize {
            self.0.load(Ordering::SeqCst)
        }
        fn remove(&self, _key: &str) -> bool {
            false
        }
    }

    #[test]
    fn slot_round_trip_returns_the_same_coordinator() {
        let counter = Arc::new(Counter::default());
        let slot = Some(into_slot(counter.clone() as Arc<dyn IdleInjectionCoordinator>));
        let coordinator = from_slot(&slot).expect("coordinator in slot");
        assert!(coordinator.enqueue(IdleInjection {
            key: "k".into(),
            source: IdleInjectionSource::TaskCompletion,
            custom_type: None,
            content: "c".into(),
            display: None,
            details: None,
            passive: None,
            on_flushed: None,
            on_delivery_failed: None,
        }));
        assert_eq!(counter.pending_count(), 1);
    }

    #[test]
    fn empty_or_foreign_slot_yields_none() {
        assert!(from_slot(&None).is_none());
        let foreign: Arc<dyn Any + Send + Sync> = Arc::new(7_u32);
        assert!(from_slot(&Some(foreign)).is_none());
    }
}
