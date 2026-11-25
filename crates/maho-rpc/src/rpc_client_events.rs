//! Port of senpi `packages/coding-agent/src/modes/rpc/rpc-client-events.ts`.
//!
//! The records an `RpcClient` hands to its event listeners (moved out of `rpc_client.rs` with senpi#5322).

use serde_json::Value;
use std::sync::Arc;

pub type RpcClientEvent = Value;
pub type RpcEventListener = Arc<dyn Fn(&RpcClientEvent) + Send + Sync>;

pub fn is_provider_account_event(event: &RpcClientEvent) -> bool {
    matches!(
        event.get("type").and_then(Value::as_str),
        Some("auth_accounts_changed" | "account_failover")
    )
}
