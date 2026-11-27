//! Delivery transport abstraction (`transport.ts`) and unified pipeline (`pipeline.ts`).

use std::future::Future;
use std::pin::Pin;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::address::RemoteSessionRow;
use super::types::{
    DeliveryAuthority, EffectiveLane, GatewayDeliveryDelivery, GatewayDeliveryMode,
    GatewayDeliveryResult, GatewaySender,
};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DeliveryTarget {
    Local {
        thread_id: String,
        #[serde(default)]
        all_scope: bool,
    },
    Remote {
        row: RemoteSessionRow,
    },
}

#[derive(Debug, Clone)]
pub struct OutboundDelivery {
    pub sender: GatewaySender,
    pub caller_id: String,
    pub target: DeliveryTarget,
    pub text: String,
    pub mode: GatewayDeliveryMode,
    pub expected_turn_id: Option<String>,
    pub idempotency_key: String,
    pub collapse_key: Option<String>,
    pub client_message_id: Option<String>,
    pub args: Value,
    pub inbound_hop: Option<i64>,
    pub authority: Option<DeliveryAuthority>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RelayDeliverySuccess {
    pub thread_id: String,
    pub delivery: GatewayDeliveryDelivery,
    pub deduplicated: bool,
    pub device: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_mode: Option<EffectiveLane>,
}

pub type DeliveryOutcome = GatewayDeliveryResult;

pub trait DeliveryTransport: Send + Sync {
    fn deliver<'a>(
        &'a self,
        delivery: OutboundDelivery,
    ) -> Pin<Box<dyn Future<Output = DeliveryOutcome> + Send + 'a>>;

    fn dispose(&self) {}
}
