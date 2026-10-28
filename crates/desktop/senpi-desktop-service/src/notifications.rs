//! Notification dispatch hub matching `src/service/notifications.ts`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use senpi_desktop_protocol::{AuditEvent, StopPathStatus, ENGINE_NOTIFICATIONS};
use serde_json::Value;

pub type Listener<T> = Arc<dyn Fn(T) + Send + Sync + 'static>;

#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
#[error("desktop engine notification \"{method}\" {reason}")]
pub struct DesktopNotificationError {
    pub method: String,
    pub reason: String,
}

impl DesktopNotificationError {
    pub fn new(method: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            method: method.into(),
            reason: reason.into(),
        }
    }
}

pub struct Subscription {
    id: u64,
    unsub: Box<dyn FnOnce(u64) + Send + Sync + 'static>,
}

impl Subscription {
    pub fn unsubscribe(self) {
        (self.unsub)(self.id);
    }
}

#[derive(Default)]
struct InnerHub {
    audit: HashMap<u64, Listener<AuditEvent>>,
    stop_path: HashMap<u64, Listener<StopPathStatus>>,
    errors: HashMap<u64, Listener<Arc<dyn std::error::Error + Send + Sync>>>,
}

#[derive(Clone, Default)]
pub struct NotificationHub {
    next_id: Arc<AtomicU64>,
    inner: Arc<Mutex<InnerHub>>,
}

impl NotificationHub {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn on_audit(&self, listener: impl Fn(AuditEvent) + Send + Sync + 'static) -> Subscription {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let mut inner = self.inner.lock().unwrap();
        inner.audit.insert(id, Arc::new(listener));
        let inner_clone = Arc::clone(&self.inner);
        Subscription {
            id,
            unsub: Box::new(move |sub_id| {
                if let Ok(mut g) = inner_clone.lock() {
                    g.audit.remove(&sub_id);
                }
            }),
        }
    }

    pub fn on_stop_path_change(
        &self,
        listener: impl Fn(StopPathStatus) + Send + Sync + 'static,
    ) -> Subscription {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let mut inner = self.inner.lock().unwrap();
        inner.stop_path.insert(id, Arc::new(listener));
        let inner_clone = Arc::clone(&self.inner);
        Subscription {
            id,
            unsub: Box::new(move |sub_id| {
                if let Ok(mut g) = inner_clone.lock() {
                    g.stop_path.remove(&sub_id);
                }
            }),
        }
    }

    pub fn on_error(
        &self,
        listener: impl Fn(Arc<dyn std::error::Error + Send + Sync>) + Send + Sync + 'static,
    ) -> Subscription {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let mut inner = self.inner.lock().unwrap();
        inner.errors.insert(id, Arc::new(listener));
        let inner_clone = Arc::clone(&self.inner);
        Subscription {
            id,
            unsub: Box::new(move |sub_id| {
                if let Ok(mut g) = inner_clone.lock() {
                    g.errors.remove(&sub_id);
                }
            }),
        }
    }

    pub fn emit_error(&self, error: Arc<dyn std::error::Error + Send + Sync>) {
        let listeners: Vec<_> = {
            let inner = self.inner.lock().unwrap();
            inner.errors.values().cloned().collect()
        };
        for listener in listeners {
            listener(Arc::clone(&error));
        }
    }

    pub fn dispatch(&self, method: &str, params: &Value) {
        if !ENGINE_NOTIFICATIONS.contains(&method) {
            self.emit_error(Arc::new(DesktopNotificationError::new(
                method,
                "is not part of the engine protocol",
            )));
            return;
        }

        match method {
            "audit" => match serde_json::from_value::<AuditEvent>(params.clone()).ok().filter(|_| crate::parse::is_audit_event(params)) {
                Some(event) => {
                    let listeners: Vec<_> = {
                        let inner = self.inner.lock().unwrap();
                        inner.audit.values().cloned().collect()
                    };
                    for listener in listeners {
                        listener(event.clone());
                    }
                }
                None => {
                    self.emit_error(Arc::new(DesktopNotificationError::new(method, "is malformed")));
                }
            },
            "stopPath.changed" => match serde_json::from_value::<StopPathStatus>(params.clone()) {
                Ok(status) => {
                    let listeners: Vec<_> = {
                        let inner = self.inner.lock().unwrap();
                        inner.stop_path.values().cloned().collect()
                    };
                    for listener in listeners {
                        listener(status.clone());
                    }
                }
                Err(_) => {
                    self.emit_error(Arc::new(DesktopNotificationError::new(method, "is malformed")));
                }
            },
            "engine.log" => {}
            _ => {
                self.emit_error(Arc::new(DesktopNotificationError::new(
                    method,
                    "is not part of the engine protocol",
                )));
            }
        }
    }
}
