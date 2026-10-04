use std::{path::PathBuf,sync::{Arc,Mutex}};
use serde::{Deserialize,Serialize};
use crate::{catalog_cache::McpCachedServerCatalog,config_schema::{McpServerSource,McpServerState},connection::{ServerConnection,ServerConnectionState},log::McpLogger};
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

#[derive(Debug,Clone,Copy,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="lowercase")]
pub enum McpSnapshotConfigState {Enabled,Disabled,Untrusted,Removed}
impl From<McpServerState> for McpSnapshotConfigState {
    fn from(state:McpServerState)->Self {match state {McpServerState::Enabled=>Self::Enabled,McpServerState::Disabled=>Self::Disabled,McpServerState::Untrusted=>Self::Untrusted}}
}
#[derive(Debug,Clone,Copy,PartialEq,Eq,Serialize,Deserialize)]
#[serde(untagged)]
pub enum McpLifecycleState {Connection(ServerConnectionState),Other(McpUnspawnedState)}
#[derive(Debug,Clone,Copy,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="snake_case")]
pub enum McpUnspawnedState {Cached,NotSpawned}
#[derive(Debug,Clone,Default,PartialEq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct McpServerCounters {pub call_count:u64,pub error_count:u64,pub total_latency_ms:f64,pub reconnect_count:u64}
#[derive(Debug,Clone,PartialEq,Serialize,Deserialize)]
#[serde(rename_all="camelCase")]
pub struct McpServerSnapshot {
    pub name:String,pub config_state:McpSnapshotConfigState,pub config_hash:Option<String>,pub source:Option<McpServerSource>,pub source_path:Option<PathBuf>,
    pub lifecycle_state:McpLifecycleState,pub generation:Option<u64>,pub pid:Option<u32>,pub last_error:Option<String>,pub uptime_ms:Option<f64>,pub counters:McpServerCounters,
}
pub struct McpConnectionEntry {
    pub key:String,pub name:String,pub config_hash:String,pub connection:Arc<ServerConnection>,pub logger:Arc<Mutex<McpLogger>>,
    pub created_at_ms:f64,pub counters:McpServerCounters,pub agent_dir:Option<PathBuf>,pub cached_catalog:Option<McpCachedServerCatalog>,pub cache_refreshed_after_connect:bool,
    pub auth_plan:crate::auth::context::ServerAuthPlan,
    pub artifacts:Option<Arc<crate::guard::output_guard::McpOutputArtifacts>>,
}
