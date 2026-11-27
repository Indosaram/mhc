//! Port of senpi `packages/coding-agent/src/modes/rpc/session-worker-data.ts`.
//!
//! Per-session-Worker data an extension host provides before the first session Worker spawns. Every
//! session Worker gets its own value from each provider, never a shared or broadcast one.

use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

pub struct SessionWorkerDataGrant {
    pub value: Value,
    pub dispose: Option<Box<dyn FnOnce() + Send>>,
}

pub type SessionWorkerDataProvider =
    Arc<dyn Fn() -> Result<SessionWorkerDataGrant, String> + Send + Sync>;

pub struct IssuedSessionWorkerData {
    pub extension_data: HashMap<String, Value>,
    disposers: Mutex<Vec<Box<dyn FnOnce() + Send>>>,
    disposed: std::sync::atomic::AtomicBool,
}

impl IssuedSessionWorkerData {
    pub fn dispose(&self) {
        if self
            .disposed
            .swap(true, std::sync::atomic::Ordering::AcqRel)
        {
            return;
        }
        let disposers = std::mem::take(
            &mut *self
                .disposers
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        for d in disposers {
            d();
        }
    }
}

impl Drop for IssuedSessionWorkerData {
    fn drop(&mut self) {
        self.dispose();
    }
}

#[derive(Default)]
struct RegistryState {
    providers: HashMap<String, SessionWorkerDataProvider>,
    order: Vec<String>,
    sealed: bool,
}

pub struct SessionWorkerDataRegistry {
    state: Mutex<RegistryState>,
    is_main_thread: Arc<dyn Fn() -> bool + Send + Sync>,
}

impl Default for SessionWorkerDataRegistry {
    fn default() -> Self {
        Self::new(Arc::new(|| true))
    }
}

impl SessionWorkerDataRegistry {
    pub fn new(is_main_thread: Arc<dyn Fn() -> bool + Send + Sync>) -> Self {
        Self {
            state: Mutex::new(RegistryState::default()),
            is_main_thread,
        }
    }

    pub fn provide(&self, name: &str, create: SessionWorkerDataProvider) -> Result<(), String> {
        if !(self.is_main_thread)() {
            return Err("provideSessionWorkerData runs on the main thread only".to_owned());
        }
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.sealed {
            return Err(format!(
                "session worker data \"{name}\" was provided after the first session worker"
            ));
        }
        if state.providers.contains_key(name) {
            return Err(format!(
                "session worker data \"{name}\" is already provided"
            ));
        }
        state.providers.insert(name.to_owned(), create);
        state.order.push(name.to_owned());
        Ok(())
    }

    pub fn spawn<T, F>(&self, start: F) -> Result<(T, Arc<IssuedSessionWorkerData>), String>
    where
        F: FnOnce(&IssuedSessionWorkerData) -> Result<T, String>,
    {
        let providers: Vec<(String, SessionWorkerDataProvider)> = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.sealed = true;
            state
                .order
                .iter()
                .filter_map(|name| state.providers.get(name).map(|p| (name.clone(), p.clone())))
                .collect()
        };
        let mut disposers: Vec<Box<dyn FnOnce() + Send>> = Vec::new();
        let mut extension_data = HashMap::new();
        for (name, create) in providers {
            match create() {
                Ok(mut grant) => {
                    if let Some(d) = grant.dispose.take() {
                        disposers.push(d);
                    }
                    extension_data.insert(name, grant.value);
                }
                Err(err) => {
                    for d in disposers {
                        d();
                    }
                    return Err(err);
                }
            }
        }
        let issued = Arc::new(IssuedSessionWorkerData {
            extension_data,
            disposers: Mutex::new(disposers),
            disposed: std::sync::atomic::AtomicBool::new(false),
        });
        match start(&issued) {
            Ok(result) => Ok((result, issued)),
            Err(err) => {
                issued.dispose();
                Err(err)
            }
        }
    }
}

static GLOBAL_REGISTRY: OnceLock<SessionWorkerDataRegistry> = OnceLock::new();

pub fn session_worker_data_registry() -> &'static SessionWorkerDataRegistry {
    GLOBAL_REGISTRY.get_or_init(SessionWorkerDataRegistry::default)
}

pub fn provide_session_worker_data(
    name: &str,
    create: SessionWorkerDataProvider,
) -> Result<(), String> {
    session_worker_data_registry().provide(name, create)
}

pub fn read_session_worker_data(data: &Value, name: &str) -> Option<Value> {
    data.as_object()?
        .get("extensionData")?
        .as_object()?
        .get(name)
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn provide_seals_on_spawn_and_disposes_on_start_failure() {
        let reg = SessionWorkerDataRegistry::default();
        let disposed = Arc::new(AtomicUsize::new(0));
        let d1 = disposed.clone();
        reg.provide(
            "ext-a",
            Arc::new(move || {
                let d1 = d1.clone();
                Ok(SessionWorkerDataGrant {
                    value: json!({ "n": 1 }),
                    dispose: Some(Box::new(move || {
                        d1.fetch_add(1, Ordering::SeqCst);
                    })),
                })
            }),
        )
        .expect("provide");
        assert!(
            reg.provide("ext-a", Arc::new(|| Err("dup".into())))
                .is_err()
        );

        let err = match reg.spawn::<(), _>(|issued| {
            assert_eq!(issued.extension_data.get("ext-a"), Some(&json!({ "n": 1 })));
            Err("spawn_failed".into())
        }) {
            Err(e) => e,
            Ok(_) => panic!("expected spawn_failed"),
        };
        assert_eq!(err, "spawn_failed");
        assert_eq!(disposed.load(Ordering::SeqCst), 1);

        // Sealed after spawn.
        assert!(
            reg.provide("ext-b", Arc::new(|| Err("late".into())))
                .is_err()
        );
    }

    #[test]
    fn read_session_worker_data_looks_up_only_own_properties() {
        let data = json!({ "extensionData": { "a": 42 } });
        assert_eq!(read_session_worker_data(&data, "a"), Some(json!(42)));
        assert_eq!(read_session_worker_data(&data, "missing"), None);
    }
}
