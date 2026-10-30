//! The public admission authority on the supervisor's loop (senpi `rpc/host-admission.ts`). A ready probe
//! is informational: every open must still pass this gate, including on held sockets.
use serde_json::{Map, Value, json};
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

type StopListener = Arc<dyn Fn() -> Pin<Box<dyn Future<Output = ()> + Send>> + Send + Sync>;

/// How long `flush` waits for the stop replies; a stalled reader cannot pin exit.
const FLUSH_TIMEOUT_MS: u64 = 2_000;

#[derive(Default)]
struct AdmissionState {
    stopped: bool,
    next_listener: u64,
    listeners: Vec<(u64, StopListener)>,
    replies: Option<tokio::task::JoinHandle<()>>,
}

#[derive(Clone)]
pub struct HostAdmission {
    info: Arc<Mutex<Map<String, Value>>>,
    state: Arc<Mutex<AdmissionState>>,
}

impl HostAdmission {
    pub fn new(info: Map<String, Value>) -> Self {
        Self {
            info: Arc::new(Mutex::new(info)),
            state: Arc::default(),
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, AdmissionState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn stopping(&self) -> bool {
        self.state().stopped
    }

    pub fn protocol(&self) -> Map<String, Value> {
        let mut info = self
            .info
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        let lifecycle = if self.stopping() { "stopping" } else { "ready" };
        info.insert("lifecycle".into(), lifecycle.into());
        info
    }

    pub fn observe(&self, info: Map<String, Value>) {
        *self
            .info
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = info;
    }

    /// Synchronous and monotonic; runs before any child signal or awaited teardown. Must be called inside
    /// a tokio runtime when stop listeners are registered.
    pub fn stop(&self) {
        let mut state = self.state();
        if state.stopped {
            return;
        }
        state.stopped = true;
        let replies: Vec<_> = state
            .listeners
            .iter()
            .map(|(_, listener)| listener())
            .collect();
        if !replies.is_empty() {
            state.replies = Some(tokio::spawn(async move {
                futures::future::join_all(replies).await;
            }));
        }
    }

    /// Flush within the caller's existing five-second cleanup allowance.
    pub async fn flush(&self) {
        let replies = self.state().replies.take();
        if let Some(replies) = replies {
            let _ = tokio::time::timeout(Duration::from_millis(FLUSH_TIMEOUT_MS), replies).await;
        }
    }

    /// Registers a stop listener; the returned closure removes it.
    pub fn on_stop<F, Fut>(&self, listener: F) -> impl FnOnce() + use<F, Fut>
    where
        F: Fn() -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let id = {
            let mut state = self.state();
            state.next_listener += 1;
            let id = state.next_listener;
            let listener: StopListener =
                Arc::new(move || -> Pin<Box<dyn Future<Output = ()> + Send>> {
                    Box::pin(listener())
                });
            state.listeners.push((id, listener));
            id
        };
        let state = self.state.clone();
        move || {
            state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .listeners
                .retain(|(existing, _)| *existing != id);
        }
    }
}

pub fn host_stopping_response(
    id: Option<&Value>,
    command: &str,
    admission: &HostAdmission,
) -> Value {
    let mut response = Map::new();
    if let Some(Value::String(id)) = id {
        response.insert("id".into(), id.clone().into());
    }
    response.insert("type".into(), "response".into());
    response.insert("command".into(), command.into());
    response.insert("success".into(), false.into());
    response.insert("error".into(), "host_stopping".into());
    response.insert("errorCode".into(), "host_stopping".into());
    let instance_id = admission.protocol().get("instanceId").cloned();
    let error_data = match instance_id {
        Some(instance_id) => json!({ "instanceId": instance_id }),
        None => json!({}),
    };
    response.insert("errorData".into(), error_data);
    Value::Object(response)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn stop_is_monotonic_and_runs_each_listener_once() {
        let admission =
            HostAdmission::new(Map::from_iter([("instanceId".to_owned(), json!("i1"))]));
        assert_eq!(admission.protocol()["lifecycle"], "ready");
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = calls.clone();
        let _remove = admission.on_stop(move || {
            let counted = counted.clone();
            async move {
                counted.fetch_add(1, Ordering::SeqCst);
            }
        });
        let removed = admission.on_stop(|| async {
            panic!("removed listener ran");
        });
        removed();
        admission.stop();
        admission.stop();
        admission.flush().await;
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(admission.stopping());
        assert_eq!(admission.protocol()["lifecycle"], "stopping");
    }

    #[test]
    fn stopping_response_names_the_instance() {
        let admission =
            HostAdmission::new(Map::from_iter([("instanceId".to_owned(), json!("i1"))]));
        let response = host_stopping_response(Some(&json!("r1")), "open_session", &admission);
        assert_eq!(
            response,
            json!({"id":"r1","type":"response","command":"open_session","success":false,"error":"host_stopping","errorCode":"host_stopping","errorData":{"instanceId":"i1"}})
        );
        let anonymous = host_stopping_response(Some(&json!(7)), "x", &admission);
        assert!(anonymous.get("id").is_none());
    }
}
