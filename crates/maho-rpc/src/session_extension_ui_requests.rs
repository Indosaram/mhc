use serde_json::{Value, json};
use std::collections::HashMap;
use tokio::sync::oneshot;

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("Extension UI request cancelled: session closed")]
pub struct SessionClosed;
pub type UiRequestResult = Result<Value, SessionClosed>;

/// A dialog appearing or ending (senpi `UiRequestChange`): what dialog observers are told.
#[derive(Clone, Debug, PartialEq)]
pub enum UiRequestChange {
    /// `ui_request_pending { uiRequestId, method, request }`: a request with a frame was registered.
    Pending {
        ui_request_id: String,
        method: Value,
        request: Value,
    },
    /// `ui_request_settled { uiRequestId, disposition }`, disposition `answered` or `cancelled`.
    Settled {
        ui_request_id: String,
        disposition: &'static str,
    },
}

impl UiRequestChange {
    /// The wire record, without the `sessionId` the writer adds (senpi `RpcUiRequestEvent`).
    pub fn record(&self) -> Value {
        match self {
            Self::Pending {
                ui_request_id,
                method,
                request,
            } => {
                json!({"type":"ui_request_pending","uiRequestId":ui_request_id,"method":method,"request":request})
            }
            Self::Settled {
                ui_request_id,
                disposition,
            } => {
                json!({"type":"ui_request_settled","uiRequestId":ui_request_id,"disposition":disposition})
            }
        }
    }
}

pub type UiRequestChanged = Box<dyn Fn(Option<UiRequestChange>) + Send + Sync>;

#[derive(Default)]
pub struct SessionExtensionUiRequests {
    grant_connections: HashMap<String, Option<String>>,
    pending: HashMap<String, oneshot::Sender<UiRequestResult>>,
    frames: HashMap<String, Value>,
    order: Vec<String>,
    changed: Option<UiRequestChanged>,
}

impl SessionExtensionUiRequests {
    /// A request set that reports every change (senpi `new SessionExtensionUiRequests(changed)`).
    pub fn with_changed(changed: UiRequestChanged) -> Self {
        Self {
            changed: Some(changed),
            ..Self::default()
        }
    }

    /// Installs the change listener after construction (a hosted session's binding learns its routing id late).
    pub fn set_changed(&mut self, changed: UiRequestChanged) {
        self.changed = Some(changed);
    }
    fn notify(&self, change: Option<UiRequestChange>) {
        if let Some(changed) = &self.changed {
            changed(change);
        }
    }

    pub fn set(&mut self, id: String, request: oneshot::Sender<UiRequestResult>) {
        self.pending.insert(id, request);
        self.notify(None);
    }

    /// Registers a request together with the frame a late attacher needs replayed (senpi `set(id, {frame})`).
    pub fn set_with_frame(
        &mut self,
        id: String,
        request: oneshot::Sender<UiRequestResult>,
        frame: Value,
    ) {
        self.frames.insert(id.clone(), frame.clone());
        self.order.push(id.clone());
        self.pending.insert(id.clone(), request);
        self.notify(Some(UiRequestChange::Pending {
            ui_request_id: id,
            method: frame.get("method").cloned().unwrap_or(Value::Null),
            request: frame,
        }));
    }

    /// The frames of pending requests that carry one, in insertion order.
    pub fn snapshot(&self) -> Vec<Value> {
        self.order
            .iter()
            .filter(|id| self.pending.contains_key(*id))
            .filter_map(|id| self.frames.get(id).cloned())
            .collect()
    }

    pub fn bind_grant(&mut self, id: &str, connection: Option<String>) {
        self.grant_connections.insert(id.into(), connection);
    }

    pub fn grant_authorized(&self, id: &str, connection: Option<&str>, capable: bool) -> bool {
        self.grant_connections
            .get(id)
            .is_none_or(|owner| owner.as_deref() == connection && capable)
    }

    pub fn has_grant(&self, id: &str) -> bool {
        self.grant_connections.contains_key(id)
    }

    pub fn grant_connection(&self, id: &str) -> Option<Option<&str>> {
        self.grant_connections.get(id).map(|c| c.as_deref())
    }

    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    pub fn cancel_grants_for(&mut self, connection: &str) {
        let ids = self
            .grant_connections
            .iter()
            .filter(|(_, owner)| owner.as_deref() == Some(connection))
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        for id in ids {
            self.resolve(json!({"id": id, "cancelled": true}));
        }
    }

    /// Cancels all grant requests that have no socket connection assigned (classic RPC mode).
    pub fn cancel_grants_for_classic(&mut self) {
        let ids = self
            .grant_connections
            .iter()
            .filter(|(_, owner)| owner.is_none())
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        for id in ids {
            self.resolve(json!({"id": id, "cancelled": true}));
        }
    }

    fn forget(&mut self, id: &str) {
        self.grant_connections.remove(id);
        self.frames.remove(id);
        self.order.retain(|entry| entry != id);
    }

    /// Drops a request; only one that existed notifies (settled `cancelled`).
    pub fn delete(&mut self, id: &str) {
        let existed = self.pending.remove(id).is_some();
        self.forget(id);
        if existed {
            self.notify(Some(UiRequestChange::Settled {
                ui_request_id: id.into(),
                disposition: "cancelled",
            }));
        }
    }

    pub fn resolve(&mut self, response: Value) -> bool {
        let Some(id) = response.get("id").and_then(Value::as_str) else {
            return false;
        };
        let Some(request) = self.pending.remove(id) else {
            return false;
        };
        let id = id.to_owned();
        self.forget(&id);
        let disposition = if response.get("cancelled").is_some() {
            "cancelled"
        } else {
            "answered"
        };
        self.notify(Some(UiRequestChange::Settled {
            ui_request_id: id,
            disposition,
        }));
        match request.send(Ok(response)) {
            Ok(()) | Err(_) => true,
        }
    }

    /// Settles every pending request as cancelled, one `ui_request_settled` each, never silently.
    pub fn cancel_all(&mut self) {
        for id in self.pending_ids() {
            let Some(request) = self.pending.remove(&id) else {
                continue;
            };
            self.forget(&id);
            self.notify(Some(UiRequestChange::Settled {
                ui_request_id: id.clone(),
                disposition: "cancelled",
            }));
            match request.send(Ok(
                json!({"type":"extension_ui_response","id":id,"cancelled":true}),
            )) {
                Ok(()) | Err(_) => {}
            }
        }
        self.grant_connections.clear();
    }

    pub fn close(&mut self) {
        for id in self.pending_ids() {
            let Some(request) = self.pending.remove(&id) else {
                continue;
            };
            self.forget(&id);
            self.notify(Some(UiRequestChange::Settled {
                ui_request_id: id,
                disposition: "cancelled",
            }));
            match request.send(Err(SessionClosed)) {
                Ok(()) | Err(_) => {}
            }
        }
        self.grant_connections.clear();
    }

    /// Pending ids, framed ones in insertion order first.
    fn pending_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self
            .order
            .iter()
            .filter(|id| self.pending.contains_key(*id))
            .cloned()
            .collect();
        let unframed: Vec<String> = self
            .pending
            .keys()
            .filter(|id| !ids.contains(id))
            .cloned()
            .collect();
        ids.extend(unframed);
        ids
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
    fn changes_report_pending_and_each_settlement() {
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = seen.clone();
        let mut requests = SessionExtensionUiRequests::with_changed(Box::new(move |change| {
            sink.lock()
                .unwrap()
                .push(change.map(|change| change.record()));
        }));
        let (a, _a) = oneshot::channel();
        let (b, _b) = oneshot::channel();
        requests.set_with_frame("a".into(), a, json!({"id":"a","method":"select"}));
        requests.set_with_frame("b".into(), b, json!({"id":"b","method":"confirm"}));
        assert!(requests.resolve(json!({"id":"a","value":"x"})));
        requests.delete("missing");
        requests.cancel_all();
        let seen = seen.lock().unwrap().clone();
        assert_eq!(
            seen,
            vec![
                Some(
                    json!({"type":"ui_request_pending","uiRequestId":"a","method":"select","request":{"id":"a","method":"select"}})
                ),
                Some(
                    json!({"type":"ui_request_pending","uiRequestId":"b","method":"confirm","request":{"id":"b","method":"confirm"}})
                ),
                Some(
                    json!({"type":"ui_request_settled","uiRequestId":"a","disposition":"answered"})
                ),
                Some(
                    json!({"type":"ui_request_settled","uiRequestId":"b","disposition":"cancelled"})
                ),
            ]
        );
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

    #[tokio::test]
    async fn grant_authorization_and_cancellation() {
        let mut requests = SessionExtensionUiRequests::default();
        let (tx1, rx1) = oneshot::channel();
        let (tx2, rx2) = oneshot::channel();
        let (tx3, rx3) = oneshot::channel();

        requests.set("grant-c1".into(), tx1);
        requests.bind_grant("grant-c1", Some("conn-1".into()));

        requests.set("grant-c2".into(), tx2);
        requests.bind_grant("grant-c2", Some("conn-2".into()));

        requests.set("normal-req".into(), tx3);

        // Untracked request is authorized regardless of connection
        assert!(requests.grant_authorized("normal-req", None, false));
        assert!(requests.grant_authorized("normal-req", Some("conn-1"), false));

        // Tracked request requires matching connection AND capability
        assert!(!requests.grant_authorized("grant-c1", Some("conn-1"), false)); // not capable
        assert!(!requests.grant_authorized("grant-c1", Some("conn-2"), true)); // wrong connection
        assert!(!requests.grant_authorized("grant-c1", None, true)); // no connection
        assert!(requests.grant_authorized("grant-c1", Some("conn-1"), true)); // authorized!

        // Cancel grants for conn-1 only cancels grant-c1
        requests.cancel_grants_for("conn-1");
        let res1 = rx1.await.unwrap().unwrap();
        assert_eq!(res1["id"], "grant-c1");
        assert_eq!(res1["cancelled"], true);

        // grant-c2 and normal-req are still pending
        assert_eq!(requests.pending_count(), 2);
        assert!(!requests.has_grant("grant-c1"));
        assert!(requests.has_grant("grant-c2"));

        // Clean up remaining
        requests.resolve(json!({"id": "grant-c2", "value": "approved"}));
        let res2 = rx2.await.unwrap().unwrap();
        assert_eq!(res2["value"], "approved");

        requests.resolve(json!({"id": "normal-req", "value": "done"}));
        let res3 = rx3.await.unwrap().unwrap();
        assert_eq!(res3["value"], "done");
    }

    #[tokio::test]
    async fn cancel_grants_for_classic_mode() {
        let mut requests = SessionExtensionUiRequests::default();
        let (tx_classic, rx_classic) = oneshot::channel();
        let (tx_socket, rx_socket) = oneshot::channel();
        let (tx_normal, _rx_normal) = oneshot::channel();

        requests.set("grant-classic".into(), tx_classic);
        requests.bind_grant("grant-classic", None); // classic mode: None connection

        requests.set("grant-socket".into(), tx_socket);
        requests.bind_grant("grant-socket", Some("socket-1".into()));

        requests.set("normal-dialog".into(), tx_normal); // non-grant dialog

        // Authorized checks for classic
        assert!(requests.grant_authorized("grant-classic", None, true));
        assert!(!requests.grant_authorized("grant-classic", None, false));
        assert!(!requests.grant_authorized("grant-classic", Some("socket-1"), true));

        // cancel_grants_for_classic only cancels classic grants (None owner)
        // Leaving non-grants and socket-assigned grants completely alone
        requests.cancel_grants_for_classic();
        let res_classic = rx_classic.await.unwrap().unwrap();
        assert_eq!(res_classic["id"], "grant-classic");
        assert_eq!(res_classic["cancelled"], true);

        // socket grant and normal dialog remain pending
        assert!(requests.has_grant("grant-socket"));
        assert!(!requests.has_grant("normal-dialog"));
        assert_eq!(requests.pending_count(), 2);

        requests.close();
        assert_eq!(rx_socket.await.unwrap(), Err(SessionClosed));
        assert!(requests.is_empty());
    }
}
