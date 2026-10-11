use crate::service_types::McpWireStatusSnapshot;
pub use maho_ext_api::mcp_wire::MCP_CONTROL_INVENTORY_REQUEST_EVENT;
pub const MCP_CONTROL_INVENTORY_CHANGED_EVENT:&str="senpi.rpc.mcp_inventory.changed";
pub use maho_ext_api::mcp_wire::McpControlInventoryRequest;
#[derive(Clone,serde::Serialize,serde::Deserialize)]
#[serde(rename_all="camelCase")]
pub struct McpControlInventoryChanged {pub session_id:String,pub snapshot:McpWireStatusSnapshot}
pub fn is_mcp_control_inventory_changed(value:&serde_json::Value)->bool {
    value.get("sessionId").is_some_and(serde_json::Value::is_string) && value.get("snapshot").and_then(|snapshot|snapshot.get("servers")).is_some_and(serde_json::Value::is_array)
}
