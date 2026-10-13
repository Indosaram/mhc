//! Port of senpi packages/coding-agent/src/core/event-bus.ts.

use std::collections::HashMap;

#[allow(clippy::type_complexity)]
type HandlerMap = HashMap<String, Vec<(u64, EventHandler, Option<ProviderScope>)>>;
use std::sync::{Arc, Mutex};

use maho_ai::node::provider_scope::{active_provider_scope, run_with_provider_scope, ProviderScope};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const EXTENSION_RPC_EVENT_CHANNEL: &str = "senpi:extension-rpc-event";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExtensionRpcEvent {
    pub name: String,
    pub data: Value,
}

pub type EventHandler = Arc<dyn Fn(&Value) + Send + Sync>;

#[derive(Clone, Default)]
pub struct EventBus {
    handlers: Arc<Mutex<HandlerMap>>,
    next_id: Arc<Mutex<u64>>,
}

impl EventBus {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn emit(&self, channel: &str, data: &Value) {
        let handlers = {
            let guard = self.handlers.lock().expect("event bus lock");
            guard.get(channel).cloned().unwrap_or_default()
        };
        for (_, handler, scope) in handlers {
            // `bindHandler`: a handler registered inside a provider scope runs in that scope; a
            // closed scope fails the call, which the safe wrapper logs like a handler error.
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match &scope {
                Some(scope) => run_with_provider_scope(scope, || handler(data)).map_err(|error| error.to_string()),
                None => {
                    handler(data);
                    Ok(())
                }
            }));
            match outcome {
                Ok(Ok(())) => {}
                Ok(Err(error)) => crate::output_guard::maho_write_stderr(&format!("Event handler error ({channel}): {error}\n")),
                Err(error) => crate::output_guard::maho_write_stderr(&format!("Event handler error ({channel}): {error:?}\n")),
            }
        }
    }

    pub fn on(&self, channel: &str, handler: EventHandler) -> EventSubscription {
        let id = {
            let mut next = self.next_id.lock().expect("event bus lock");
            *next += 1;
            *next
        };
        {
            let mut guard = self.handlers.lock().expect("event bus lock");
            guard.entry(channel.to_owned()).or_default().push((id, handler, active_provider_scope()));
        }
        EventSubscription { bus: self.clone(), channel: channel.to_owned(), id }
    }

    pub fn clear(&self) {
        self.handlers.lock().expect("event bus lock").clear();
    }

    fn unsubscribe(&self, channel: &str, id: u64) {
        let mut guard = self.handlers.lock().expect("event bus lock");
        if let Some(handlers) = guard.get_mut(channel) {
            handlers.retain(|(handler_id, _, _)| *handler_id != id);
            if handlers.is_empty() {
                guard.remove(channel);
            }
        }
    }
}

pub struct EventSubscription {
    bus: EventBus,
    channel: String,
    id: u64,
}

impl Drop for EventSubscription {
    fn drop(&mut self) {
        self.bus.unsubscribe(&self.channel, self.id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn delivers_to_subscribers_and_unsubscribes_on_drop() {
        let bus = EventBus::new();
        let count = Arc::new(AtomicUsize::new(0));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let subscription = {
            let count = Arc::clone(&count);
            let seen = Arc::clone(&seen);
            bus.on(
                "channel",
                Arc::new(move |data| {
                    count.fetch_add(1, Ordering::SeqCst);
                    seen.lock().expect("lock").push(data.clone());
                }),
            )
        };
        bus.emit("channel", &Value::from(1));
        assert_eq!(count.load(Ordering::SeqCst), 1);
        assert_eq!(seen.lock().expect("lock").as_slice(), &[Value::from(1)]);
        drop(subscription);
        bus.emit("channel", &Value::from(2));
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_panicking_handler_does_not_stop_other_handlers() {
        let bus = EventBus::new();
        let count = Arc::new(AtomicUsize::new(0));
        let _bad = bus.on("c", Arc::new(|_| panic!("boom")));
        let _good = {
            let count = Arc::clone(&count);
            bus.on(
                "c",
                Arc::new(move |_| {
                    count.fetch_add(1, Ordering::SeqCst);
                }),
            )
        };
        bus.emit("c", &Value::Null);
        assert_eq!(count.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn clear_removes_every_subscriber() {
        let bus = EventBus::new();
        let count = Arc::new(AtomicUsize::new(0));
        let _subscription = {
            let count = Arc::clone(&count);
            bus.on(
                "c",
                Arc::new(move |_| {
                    count.fetch_add(1, Ordering::SeqCst);
                }),
            )
        };
        bus.clear();
        bus.emit("c", &Value::Null);
        assert_eq!(count.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_handler_runs_in_the_provider_scope_it_was_registered_in() {
        let bus = EventBus::new();
        let scope = ProviderScope::new();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let _subscription = run_with_provider_scope(&scope, || {
            let seen = Arc::clone(&seen);
            bus.on("c", Arc::new(move |_| {
                seen.lock().expect("lock").push(active_provider_scope());
            }))
        })
        .expect("scope");
        bus.emit("c", &Value::Null);
        let seen = seen.lock().expect("lock");
        assert_eq!(seen.len(), 1);
        assert!(seen[0].as_ref().is_some_and(|active| active.ptr_eq(&scope)));
    }

    #[test]
    fn a_handler_bound_to_a_closed_scope_is_not_called() {
        let bus = EventBus::new();
        let scope = ProviderScope::new();
        let count = Arc::new(AtomicUsize::new(0));
        let _subscription = run_with_provider_scope(&scope, || {
            let count = Arc::clone(&count);
            bus.on("c", Arc::new(move |_| {
                count.fetch_add(1, Ordering::SeqCst);
            }))
        })
        .expect("scope");
        scope.close();
        bus.emit("c", &Value::Null);
        assert_eq!(count.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn the_rpc_channel_name_is_pinned() {
        assert_eq!(EXTENSION_RPC_EVENT_CHANNEL, "senpi:extension-rpc-event");
        let event = ExtensionRpcEvent { name: "x".to_owned(), data: Value::from(1) };
        assert_eq!(serde_json::to_value(&event).expect("json")["name"], "x");
    }
}
