//! Port of senpi `packages/coding-agent/src/core/extensions/builtin/anthropic-subscription/account-events.ts`.
//!
//! The registry lives in `maho-core` rather than the anthropic-subscription builtin because
//! `maho-core` cannot depend on the builtin (the builtin depends on `maho-core`), yet both the
//! session's account producers and the app-server consumer need one process-scoped listener set.

use std::sync::{Arc, Mutex, OnceLock};

/// Native port of the pinned `ProviderAccountEvent` union.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProviderAccountEvent {
    AccountsChanged { provider: String },
    Failover { provider: String, from: String, to: String, reason: String },
}

type Listener = Arc<dyn Fn(ProviderAccountEvent) + Send + Sync>;

/// Ordered, identity-deduplicated listener set, matching the pinned `Set<ProviderAccountEventListener>`.
#[derive(Default)]
pub struct ProviderAccountEvents {
    listeners: Mutex<Vec<Listener>>,
}

impl ProviderAccountEvents {
    /// Register `listener` and return an idempotent unsubscribe closure. Re-registering the same
    /// listener identity is a no-op, matching the pinned `Set.add`.
    pub fn subscribe(self: &Arc<Self>, listener: Listener) -> Box<dyn FnOnce() + Send + Sync> {
        {
            let mut listeners = self.listeners.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if !listeners.iter().any(|existing| Arc::ptr_eq(existing, &listener)) {
                listeners.push(listener.clone());
            }
        }
        let events = self.clone();
        Box::new(move || {
            events
                .listeners
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .retain(|existing| !Arc::ptr_eq(existing, &listener));
        })
    }

    /// Deliver `event` to every listener still registered; a listener removed mid-dispatch by an
    /// earlier listener does not receive it, matching the pinned `Set` iteration.
    pub fn emit(&self, event: ProviderAccountEvent) {
        let listeners = self.listeners.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
        for listener in listeners {
            let still_registered = self
                .listeners
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .iter()
                .any(|existing| Arc::ptr_eq(existing, &listener));
            if still_registered {
                listener(event.clone());
            }
        }
    }
}

/// The process-global registry the app-server subscribes to once.
pub fn provider_account_events() -> &'static Arc<ProviderAccountEvents> {
    static EVENTS: OnceLock<Arc<ProviderAccountEvents>> = OnceLock::new();
    EVENTS.get_or_init(|| Arc::new(ProviderAccountEvents::default()))
}

/// `subscribeProviderAccountEvents(listener)`: returns the unsubscribe closure.
pub fn subscribe_provider_account_events(listener: Listener) -> Box<dyn FnOnce() + Send + Sync> {
    provider_account_events().subscribe(listener)
}

/// `emitProviderAccountsChanged(provider)`.
pub fn emit_provider_accounts_changed(provider: &str) {
    provider_account_events().emit(ProviderAccountEvent::AccountsChanged { provider: provider.to_owned() });
}

/// `emitProviderAccountFailover(provider, from, to, reason)`.
pub fn emit_provider_account_failover(provider: &str, from: &str, to: &str, reason: &str) {
    provider_account_events().emit(ProviderAccountEvent::Failover {
        provider: provider.to_owned(),
        from: from.to_owned(),
        to: to.to_owned(),
        reason: reason.to_owned(),
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn capture() -> (Arc<Mutex<Vec<ProviderAccountEvent>>>, Listener) {
        let observed = Arc::new(Mutex::new(Vec::new()));
        let sink = observed.clone();
        (observed, Arc::new(move |event| sink.lock().expect("observed").push(event)))
    }

    #[test]
    fn subscription_is_ordered_unique_and_explicitly_removed() {
        let events = Arc::new(ProviderAccountEvents::default());
        let (observed, listener) = capture();
        let unsubscribe = events.subscribe(listener.clone());
        let duplicate = events.subscribe(listener);
        let changed = ProviderAccountEvent::AccountsChanged { provider: "provider".to_owned() };
        events.emit(changed.clone());
        assert_eq!(observed.lock().expect("observed").as_slice(), std::slice::from_ref(&changed));
        unsubscribe();
        events.emit(changed);
        duplicate();
        assert_eq!(observed.lock().expect("observed").len(), 1);
    }

    #[test]
    fn a_listener_can_unsubscribe_a_later_listener_before_it_runs() {
        let events = Arc::new(ProviderAccountEvents::default());
        let later = Arc::new(Mutex::new(None::<Box<dyn FnOnce() + Send + Sync>>));
        let calls = Arc::new(Mutex::new(Vec::new()));
        let first = {
            let later = later.clone();
            let calls = calls.clone();
            Arc::new(move |_: ProviderAccountEvent| {
                calls.lock().expect("calls").push(1);
                if let Some(remove) = later.lock().expect("later").take() {
                    remove();
                }
            }) as Listener
        };
        let remove_first = events.subscribe(first);
        let second = {
            let calls = calls.clone();
            Arc::new(move |_: ProviderAccountEvent| calls.lock().expect("calls").push(2)) as Listener
        };
        *later.lock().expect("later") = Some(events.subscribe(second));
        events.emit(ProviderAccountEvent::Failover {
            provider: "p".to_owned(), from: "a".to_owned(), to: "b".to_owned(), reason: "rate-limit".to_owned(),
        });
        assert_eq!(*calls.lock().expect("calls"), [1]);
        remove_first();
    }

    #[test]
    fn the_global_registry_delivers_then_stops_after_unsubscribe() {
        let observed = Arc::new(Mutex::new(Vec::new()));
        let sink = observed.clone();
        let unsubscribe = subscribe_provider_account_events(Arc::new(move |event| {
            if matches!(&event, ProviderAccountEvent::AccountsChanged { provider } if provider == "global-registry-fixture") {
                sink.lock().expect("observed").push(event);
            }
        }));
        emit_provider_accounts_changed("global-registry-fixture");
        unsubscribe();
        emit_provider_accounts_changed("global-registry-fixture");
        let observed = observed.lock().expect("observed");
        assert_eq!(observed.as_slice(), [ProviderAccountEvent::AccountsChanged { provider: "global-registry-fixture".to_owned() }]);
    }
}
