//! Hosted listener registration survives connection replacement.
pub type ResumedCallback = Arc<Mutex<BTreeMap<u64, Arc<dyn Fn() + Send + Sync>>>>;
pub type DurableCallback = Arc<Mutex<BTreeMap<u64, Arc<dyn Fn(&str) + Send + Sync>>>>;
use crate::manager::child_handle::Unsubscribe;
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};
#[derive(Clone, Default)]
pub struct HandleListeners {
    ids: Arc<AtomicU64>,
    events: Arc<Mutex<BTreeMap<u64, crate::manager::child_handle::ManagedChildListener>>>,
    resumed: ResumedCallback,
    durable: DurableCallback,
}
pub fn create_handle_listeners() -> HandleListeners {
    HandleListeners::default()
}
impl HandleListeners {
    pub fn on_event(
        &self,
        listener: crate::manager::child_handle::ManagedChildListener,
    ) -> Unsubscribe {
        let id = self.ids.fetch_add(1, Ordering::Relaxed);
        self.events.lock().expect("hosted child state lock poisoned").insert(id, listener);
        let listeners = self.events.clone();
        Box::new(move || {
            listeners.lock().expect("hosted child state lock poisoned").remove(&id);
        })
    }
    pub fn emit_event(&self, event: &crate::manager::child_handle::ManagedChildEvent) {
        let listeners = self
            .events
            .lock()
            .expect("hosted child state lock poisoned")
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for listener in listeners {
            listener(event);
        }
    }
    pub fn on_self_resumed(&self, listener: Arc<dyn Fn() + Send + Sync>) -> Unsubscribe {
        let id = self.ids.fetch_add(1, Ordering::Relaxed);
        self.resumed.lock().expect("hosted child state lock poisoned").insert(id, listener);
        let listeners = self.resumed.clone();
        Box::new(move || {
            listeners.lock().expect("hosted child state lock poisoned").remove(&id);
        })
    }
    pub fn emit_self_resumed(&self) {
        let listeners = self
            .resumed
            .lock()
            .expect("hosted child state lock poisoned")
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for listener in listeners {
            listener();
        }
    }
    pub fn on_durable_session_id(&self, listener: Arc<dyn Fn(&str) + Send + Sync>) -> Unsubscribe {
        let id = self.ids.fetch_add(1, Ordering::Relaxed);
        self.durable.lock().expect("hosted child state lock poisoned").insert(id, listener);
        let listeners = self.durable.clone();
        Box::new(move || {
            listeners.lock().expect("hosted child state lock poisoned").remove(&id);
        })
    }
    pub fn emit_durable_session_id(&self, id: &str) {
        let listeners = self
            .durable
            .lock()
            .expect("hosted child state lock poisoned")
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for listener in listeners {
            listener(id);
        }
    }
    pub fn clear_active(&self) {
        self.events.lock().expect("hosted child state lock poisoned").clear();
        self.resumed.lock().expect("hosted child state lock poisoned").clear();
        self.durable.lock().expect("hosted child state lock poisoned").clear();
    }
}
