//! The public admission authority on the supervisor's loop (senpi `host-admission.ts`). A ready probe is
//! informational: every open must still pass this gate, including on held sockets.
use serde_json::{Map, Value, json};
use std::sync::Mutex;

pub struct HostAdmission {
    info: Mutex<Map<String, Value>>,
    stopped: tokio::sync::watch::Sender<bool>,
    /// Refusals the stop listeners started; `flush` awaits them within a bound.
    replies: Mutex<Vec<tokio::sync::oneshot::Receiver<()>>>,
}

impl HostAdmission {
    pub fn new(info: Map<String, Value>) -> Self {
        Self {
            info: Mutex::new(info),
            stopped: tokio::sync::watch::channel(false).0,
            replies: Mutex::new(Vec::new()),
        }
    }

    pub fn stopping(&self) -> bool {
        *self.stopped.borrow()
    }

    pub fn protocol(&self) -> Map<String, Value> {
        let mut info = self
            .info
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        info.insert(
            "lifecycle".into(),
            if self.stopping() { "stopping" } else { "ready" }.into(),
        );
        info
    }

    pub fn observe(&self, info: Map<String, Value>) {
        *self
            .info
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = info;
    }

    /// Synchronous and monotonic; runs before any child signal or awaited teardown.
    pub fn stop(&self) {
        self.stopped
            .send_if_modified(|stopped| !std::mem::replace(stopped, true));
    }

    /// A stop listener (senpi `onStop`): resolves when admission stops.
    pub fn on_stop(&self) -> tokio::sync::watch::Receiver<bool> {
        self.stopped.subscribe()
    }

    /// Registers a stop listener's refusal so `flush` can wait for it.
    pub fn track_reply(&self) -> tokio::sync::oneshot::Sender<()> {
        let (done, wait) = tokio::sync::oneshot::channel();
        self.replies
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(wait);
        done
    }

    /// Flush within the caller's existing five-second cleanup allowance; a stalled reader cannot pin exit.
    pub async fn flush(&self) {
        let replies = std::mem::take(
            &mut *self
                .replies
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        let _ = tokio::time::timeout(
            std::time::Duration::from_millis(2_000),
            futures::future::join_all(replies),
        )
        .await;
    }
}

/// senpi `hostStoppingResponse`.
pub fn host_stopping_response(
    id: Option<&Value>,
    command: &str,
    admission: &HostAdmission,
) -> Value {
    let mut response = Map::new();
    if let Some(id) = id.filter(|id| id.is_string()) {
        response.insert("id".into(), id.clone());
    }
    response.insert("type".into(), "response".into());
    response.insert("command".into(), command.into());
    response.insert("success".into(), false.into());
    response.insert("error".into(), "host_stopping".into());
    response.insert("errorCode".into(), "host_stopping".into());
    response.insert(
        "errorData".into(),
        json!({"instanceId": admission.protocol().get("instanceId").cloned().unwrap_or(Value::Null)}),
    );
    Value::Object(response)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stop_is_monotonic_and_reported_in_protocol_and_refusals() {
        let admission = HostAdmission::new(
            json!({"instanceId":"i","capabilities":[]})
                .as_object()
                .cloned()
                .unwrap_or_default(),
        );
        assert_eq!(admission.protocol()["lifecycle"], "ready");
        admission.stop();
        admission.stop();
        assert!(admission.stopping());
        assert_eq!(admission.protocol()["lifecycle"], "stopping");
        assert_eq!(
            host_stopping_response(Some(&json!(7)), "open_session", &admission),
            json!({"type":"response","command":"open_session","success":false,"error":"host_stopping","errorCode":"host_stopping","errorData":{"instanceId":"i"}})
        );
    }
}
