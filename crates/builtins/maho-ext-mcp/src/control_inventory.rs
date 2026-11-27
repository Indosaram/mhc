use crate::{
    service::{McpService, McpWireStatusSubscriptions},
    service_types::McpWireStatusSnapshot,
};
pub use maho_ext_api::mcp_wire::MCP_CONTROL_INVENTORY_REQUEST_EVENT;
pub const MCP_CONTROL_INVENTORY_CHANGED_EVENT: &str = "senpi.rpc.mcp_inventory.changed";
pub use maho_ext_api::mcp_wire::McpControlInventoryRequest;
use std::{
    collections::BTreeSet,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpControlInventoryChanged {
    pub session_id: String,
    pub snapshot: McpWireStatusSnapshot,
    /// Set on the one notification sent once every MCP connect of the session's attach has settled (senpi#4828).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub startup_settled: Option<bool>,
}

pub fn is_mcp_control_inventory_changed(value: &serde_json::Value) -> bool {
    value
        .get("sessionId")
        .is_some_and(serde_json::Value::is_string)
        && value
            .get("snapshot")
            .and_then(|snapshot| snapshot.get("servers"))
            .is_some_and(serde_json::Value::is_array)
}

/// Handle returned by [`forward_mcp_control_inventory`]. Stops forwarding on drop or `dispose()`.
pub struct ControlInventoryForwarder {
    disposed: Arc<AtomicBool>,
    request_sub: Mutex<Option<maho_ext_api::BusSubscription>>,
    status_unsubscribe: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

impl ControlInventoryForwarder {
    pub fn dispose(&self) {
        if self.disposed.swap(true, Ordering::SeqCst) {
            return;
        }
        self.request_sub
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(unsub) = self
            .status_unsubscribe
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            unsub();
        }
    }
}

impl Drop for ControlInventoryForwarder {
    fn drop(&mut self) {
        self.dispose();
    }
}

pub type McpWireStatusSink = Arc<dyn Fn(&McpWireStatusSnapshot) + Send + Sync>;

/// Answer control-inventory requests and forward wire-status changes for every session in `attached`
/// (each scope a runtime attached). The returned forwarder stops both on `dispose()` or drop.
pub fn forward_mcp_control_inventory(
    events: &maho_ext_api::EventBus,
    service: &Arc<tokio::sync::Mutex<McpService>>,
    attached: Arc<Mutex<BTreeSet<String>>>,
    wire_status_sink: Option<McpWireStatusSink>,
    subscriptions: &McpWireStatusSubscriptions,
) -> ControlInventoryForwarder {
    let disposed = Arc::new(AtomicBool::new(false));
    let request_sub = {
        let service = service.clone();
        let attached = attached.clone();
        let disposed = disposed.clone();
        events.on_native::<McpControlInventoryRequest>(
            MCP_CONTROL_INVENTORY_REQUEST_EVENT,
            Arc::new(move |request| {
                if disposed.load(Ordering::SeqCst) {
                    return;
                }
                let is_attached = attached
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .contains(&request.session_id);
                if !is_attached {
                    return;
                }
                let sender = request
                    .respond
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .take();
                let service = service.clone();
                let session_id = request.session_id.clone();
                if let Some(sender) = sender {
                    tokio::spawn(async move {
                        let snapshot = service
                            .lock()
                            .await
                            .refresh_wire_status_snapshot(Some(&session_id))
                            .await;
                        match serde_json::to_value(snapshot) {
                            Ok(snapshot) => {
                                if sender.send(snapshot).is_err() {
                                    /* Requesting connection was disposed. */
                                }
                            }
                            Err(error) => maho_ext_api::diagnostic(&format!(
                                "MCP inventory serialization failed: {error}"
                            )),
                        }
                    });
                }
            }),
        )
    };
    let status_unsubscribe = {
        let events = events.clone();
        let disposed = disposed.clone();
        Some(
            subscriptions.subscribe(Arc::new(move |session_id, snapshot, startup_settled| {
                if disposed.load(Ordering::SeqCst) {
                    return;
                }
                let Some(session_id) = session_id else {
                    return;
                };
                let is_attached = attached
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .contains(session_id);
                if !is_attached {
                    return;
                }
                let payload = McpControlInventoryChanged {
                    session_id: session_id.to_owned(),
                    snapshot: snapshot.clone(),
                    startup_settled: startup_settled.then_some(true),
                };
                if let Ok(value) = serde_json::to_value(&payload) {
                    events.emit(MCP_CONTROL_INVENTORY_CHANGED_EVENT, &value);
                }
                if let Some(sink) = &wire_status_sink {
                    sink(snapshot);
                }
            })),
        )
    };
    ControlInventoryForwarder {
        disposed,
        request_sub: Mutex::new(Some(request_sub)),
        status_unsubscribe: Mutex::new(status_unsubscribe),
    }
}
