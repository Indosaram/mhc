use serde::{Deserialize, Serialize};

#[derive(Debug,Clone,Copy,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="snake_case")]
pub enum ServerConnectionState {Disabled,Idle,Connecting,Connected,Degraded,Suspended,NeedsAuth,NeedsClientRegistration}
#[derive(Debug,Clone,Copy,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="lowercase")]
pub enum McpServerState {Enabled,Disabled,Untrusted}
#[derive(Debug,Clone,Copy,PartialEq,Eq,Serialize,Deserialize)]
pub enum McpWireAuthStatus {#[serde(rename="unsupported")] Unsupported,#[serde(rename="notLoggedIn")] NotLoggedIn,#[serde(rename="bearerToken")] BearerToken,#[serde(rename="oAuth")] OAuth}
#[derive(Debug,Clone,PartialEq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct McpWireServerInfo {pub name:String,pub title:Option<String>,pub version:String,pub description:Option<String>,pub icons:Option<Vec<serde_json::Value>>,pub website_url:Option<String>}
#[derive(Debug,Clone,PartialEq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct McpWireTool {pub name:String,pub input_schema:serde_json::Value,#[serde(flatten)] pub extra:std::collections::BTreeMap<String,serde_json::Value>}
#[derive(Debug,Clone,PartialEq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct McpWireResource {pub name:String,pub uri:String,#[serde(flatten)] pub extra:std::collections::BTreeMap<String,serde_json::Value>}
#[derive(Debug,Clone,PartialEq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct McpWireResourceTemplate {pub name:String,pub uri_template:String,#[serde(flatten)] pub extra:std::collections::BTreeMap<String,serde_json::Value>}
#[derive(Debug,Clone,PartialEq,Serialize,Deserialize)]
#[serde(untagged)]
pub enum McpWireServerStatus {Connection(ServerConnectionState),Config(McpServerState)}
#[derive(Debug,Clone,PartialEq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct McpWireStatusServer {pub name:String,pub server_info:Option<McpWireServerInfo>,pub tools:Vec<McpWireTool>,pub resources:Vec<McpWireResource>,pub resource_templates:Vec<McpWireResourceTemplate>,pub auth_status:McpWireAuthStatus,#[serde(skip_serializing_if="Option::is_none")] pub status:Option<McpWireServerStatus>}
#[derive(Debug,Clone,Default,PartialEq,Serialize,Deserialize)]
pub struct McpWireStatusSnapshot {pub servers:Vec<McpWireStatusServer>}

pub const MCP_CONTROL_INVENTORY_REQUEST_EVENT: &str = "senpi.rpc.mcp_inventory.request";
pub struct McpControlInventoryRequest {
    pub session_id: String,
    pub respond: std::sync::Mutex<Option<tokio::sync::oneshot::Sender<serde_json::Value>>>,
}
