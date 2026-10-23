use serde_json::{Value, json};
use std::collections::HashMap;
use tokio::sync::oneshot;

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("Extension UI request cancelled: session closed")]
pub struct SessionClosed;
pub type UiRequestResult = Result<Value, SessionClosed>;
#[derive(Default)]
pub struct SessionExtensionUiRequests {
    pending: HashMap<String, oneshot::Sender<UiRequestResult>>,
    frames: HashMap<String, Value>,
    order: Vec<String>,
}
impl SessionExtensionUiRequests {
    pub fn set(&mut self, id: String, request: oneshot::Sender<UiRequestResult>) {
        self.pending.insert(id, request);
    }
    /// Registers a request together with the frame a late attacher needs replayed (senpi `set(id, {frame})`).
    pub fn set_with_frame(
        &mut self,
        id: String,
        request: oneshot::Sender<UiRequestResult>,
        frame: Value,
    ) {
        self.frames.insert(id.clone(), frame);
        self.order.push(id.clone());
        self.pending.insert(id, request);
    }
    /// The frames of pending requests that carry one, in insertion order.
    pub fn snapshot(&self) -> Vec<Value> {
        self.order
            .iter()
            .filter(|id| self.pending.contains_key(*id))
            .filter_map(|id| self.frames.get(id).cloned())
            .collect()
    }
    fn forget(&mut self, id: &str) {
        self.frames.remove(id);
        self.order.retain(|entry| entry != id);
    }
    pub fn delete(&mut self, id: &str) {
        self.pending.remove(id);
        self.forget(id);
    }
    pub fn resolve(&mut self, response: Value) -> bool {
        let Some(id) = response.get("id").and_then(Value::as_str) else {
            return false;
        };
        let Some(request) = self.pending.remove(id) else {
            return false;
        };
        self.forget(id);
        match request.send(Ok(response)) {
            Ok(()) | Err(_) => true,
        }
    }
    pub fn cancel_all(&mut self) {
        self.frames.clear();
        self.order.clear();
        for (id, request) in self.pending.drain() {
            match request.send(Ok(
                json!({"type":"extension_ui_response","id":id,"cancelled":true}),
            )) {
                Ok(()) | Err(_) => {}
            }
        }
    }
    pub fn close(&mut self) {
        self.frames.clear();
        self.order.clear();
        for (_, request) in self.pending.drain() {
            match request.send(Err(SessionClosed)) {
                Ok(()) | Err(_) => {}
            }
        }
    }
}
/// Per-session pending-request sets (senpi `sessionDialogRequests`, a `WeakMap` keyed by session):
/// keyed by session id here, dropped with [`SessionDialogRequests::forget`] when the session ends.
#[derive(Default)]
pub struct SessionDialogRequests {
    sessions: HashMap<String, std::sync::Arc<std::sync::Mutex<SessionExtensionUiRequests>>>,
}
impl SessionDialogRequests {
    pub fn of(
        &mut self,
        session: &str,
    ) -> std::sync::Arc<std::sync::Mutex<SessionExtensionUiRequests>> {
        self.sessions.entry(session.to_owned()).or_default().clone()
    }
    pub fn forget(&mut self, session: &str) {
        self.sessions.remove(session);
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn response_resolves_only_matching_session_request() {
        let mut first = SessionExtensionUiRequests::default();
        let mut second = SessionExtensionUiRequests::default();
        let (sender, receiver) = oneshot::channel();
        first.set("id".into(), sender);
        let response = json!({"type":"extension_ui_response","id":"id","value":"ok"});
        assert!(!second.resolve(response.clone()));
        assert!(first.resolve(response.clone()));
        assert_eq!(receiver.await.unwrap().unwrap(), response);
    }
    #[tokio::test]
    async fn cancellation_resolves_all_requests_with_cancelled_records() {
        let mut requests = SessionExtensionUiRequests::default();
        let (sender, receiver) = oneshot::channel();
        requests.set("id".into(), sender);
        requests.cancel_all();
        assert_eq!(
            receiver.await.unwrap().unwrap(),
            json!({"type":"extension_ui_response","id":"id","cancelled":true})
        );
        assert!(!requests.resolve(json!({"id":"id"})));
    }
    #[tokio::test]
    async fn session_close_rejects_pending_request() {
        let mut requests = SessionExtensionUiRequests::default();
        let (sender, receiver) = oneshot::channel();
        requests.set("id".into(), sender);
        requests.close();
        assert_eq!(receiver.await.unwrap(), Err(SessionClosed));
    }
    #[test]
    fn deleted_request_is_not_resolved() {
        let mut requests = SessionExtensionUiRequests::default();
        let (sender, _receiver) = oneshot::channel();
        requests.set("id".into(), sender);
        requests.delete("id");
        assert!(!requests.resolve(json!({"id":"id"})));
    }
    #[test]
    fn snapshot_lists_pending_frames_in_insertion_order() {
        let mut requests = SessionExtensionUiRequests::default();
        let (first, _first_rx) = oneshot::channel();
        let (second, _second_rx) = oneshot::channel();
        let (bare, _bare_rx) = oneshot::channel();
        requests.set_with_frame("a".into(), first, json!({"id":"a"}));
        requests.set("b".into(), bare);
        requests.set_with_frame("c".into(), second, json!({"id":"c"}));
        assert_eq!(
            requests.snapshot(),
            vec![json!({"id":"a"}), json!({"id":"c"})]
        );
        requests.delete("a");
        assert_eq!(requests.snapshot(), vec![json!({"id":"c"})]);
        assert!(requests.resolve(json!({"id":"c"})));
        assert!(requests.snapshot().is_empty());
    }
}
