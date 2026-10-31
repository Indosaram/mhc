//! Port of `cache-keepalive/startup-activity-gate.ts` (senpi 903b8336).
//!
//! Upstream subscribes through `ctx.onUserActivity`; the subscription is a caller-supplied seam
//! (`subscribe`, the extension wiring passes `ExtensionContext::on_user_activity`) that returns the
//! unsubscribe hook, or `None` when the host cannot report activity (upstream's optional-chained
//! `undefined`).

use std::sync::{Arc, Mutex, PoisonError};

pub type Unsubscribe = Box<dyn FnOnce() + Send>;
pub type ActivityListener = Box<dyn Fn() + Send + Sync>;

#[derive(Default)]
struct GateState {
    waiting: bool,
    unsubscribe: Option<Unsubscribe>,
}

/// Restored history is not admission; real activity or a completed turn is.
#[derive(Clone, Default)]
pub struct StartupActivityGate {
    state: Arc<Mutex<GateState>>,
}

pub fn create_startup_activity_gate() -> StartupActivityGate {
    StartupActivityGate::default()
}

impl StartupActivityGate {
    pub fn waiting(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .waiting
    }

    pub fn release(&self) {
        let unsubscribe = {
            let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            state.waiting = false;
            state.unsubscribe.take()
        };
        if let Some(unsubscribe) = unsubscribe {
            unsubscribe();
        }
    }

    pub fn start(
        &self,
        subscribe: impl FnOnce(ActivityListener) -> Option<Unsubscribe>,
        on_activity: impl Fn() + Send + Sync + 'static,
    ) {
        self.release();
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .waiting = true;
        let gate = self.clone();
        let unsubscribe = subscribe(Box::new(move || {
            gate.release();
            on_activity();
        }));
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        // An activity delivered synchronously during subscribe already released the gate.
        if state.waiting {
            state.unsubscribe = unsubscribe;
        } else {
            drop(state);
            if let Some(unsubscribe) = unsubscribe {
                unsubscribe();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    type Listeners = Arc<Mutex<Vec<(usize, Arc<ActivityListener>)>>>;

    fn host() -> (
        Listeners,
        impl Fn() -> Box<dyn FnOnce(ActivityListener) -> Option<Unsubscribe>>,
    ) {
        let listeners: Listeners = Arc::default();
        let next = Arc::new(AtomicUsize::new(0));
        let shared = listeners.clone();
        let subscribe = move || {
            let listeners = shared.clone();
            let next = next.clone();
            Box::new(move |listener: ActivityListener| {
                let id = next.fetch_add(1, Ordering::SeqCst);
                listeners.lock().unwrap().push((id, Arc::new(listener)));
                let listeners = listeners.clone();
                Some(Box::new(move || {
                    listeners
                        .lock()
                        .unwrap()
                        .retain(|(existing, _)| *existing != id)
                }) as Unsubscribe)
            }) as Box<dyn FnOnce(ActivityListener) -> Option<Unsubscribe>>
        };
        (listeners, subscribe)
    }

    fn fire(listeners: &Listeners) {
        let current: Vec<_> = listeners
            .lock()
            .unwrap()
            .iter()
            .map(|(_, listener)| listener.clone())
            .collect();
        for listener in current {
            listener();
        }
    }

    #[test]
    fn waits_until_activity_then_releases_once() {
        let (listeners, subscribe) = host();
        let gate = create_startup_activity_gate();
        assert!(!gate.waiting());
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = calls.clone();
        gate.start(subscribe(), move || {
            counted.fetch_add(1, Ordering::SeqCst);
        });
        assert!(gate.waiting());
        fire(&listeners);
        assert!(!gate.waiting());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(listeners.lock().unwrap().is_empty());
        fire(&listeners);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn restart_replaces_the_previous_subscription() {
        let (listeners, subscribe) = host();
        let gate = create_startup_activity_gate();
        gate.start(subscribe(), || {});
        gate.start(subscribe(), || {});
        assert_eq!(listeners.lock().unwrap().len(), 1);
        gate.release();
        assert!(!gate.waiting());
        assert!(listeners.lock().unwrap().is_empty());
    }

    #[test]
    fn host_without_activity_keeps_waiting() {
        let gate = create_startup_activity_gate();
        gate.start(|_| None, || {});
        assert!(gate.waiting());
        gate.release();
        assert!(!gate.waiting());
    }
}
