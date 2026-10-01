use crate::types::{PermissionDecision, Request};
use maho_ext_api::{BusSubscription, EventBus};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PermissionRepliedEvent {
    #[serde(rename = "requestID")]
    pub request_id: String,
    #[serde(rename = "sessionID")]
    pub session_id: String,
    pub reply: PermissionDecision,
}
#[derive(Clone, Default)]
pub struct PermissionEventEmitter { pub bus: EventBus }
impl PermissionEventEmitter {
    pub fn emit_asked(&self, request: &Request) -> Result<(), serde_json::Error> {
        self.bus.emit("permission_asked",&serde_json::to_value(request)?); Ok(())
    }
    pub fn emit_replied(&self, event: PermissionRepliedEvent) -> Result<(), serde_json::Error> {
        self.bus.emit("permission_replied",&serde_json::to_value(event)?); Ok(())
    }
    pub fn on_asked(&self, handler: impl Fn(Request) + Send + Sync + 'static) -> BusSubscription {
        self.bus.on("permission_asked",Arc::new(move |value| {
            if let Ok(request)=serde_json::from_value(value.clone()) { handler(request); }
        }))
    }
    pub fn on_replied(&self, handler: impl Fn(PermissionRepliedEvent) + Send + Sync + 'static) -> BusSubscription {
        self.bus.on("permission_replied",Arc::new(move |value| {
            if let Ok(event)=serde_json::from_value(value.clone()) { handler(event); }
        }))
    }
    pub fn clear(&self) { self.bus.clear(); }
}
