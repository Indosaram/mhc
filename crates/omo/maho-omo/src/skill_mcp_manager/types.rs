use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::BTreeMap, future::Future, pin::Pin, sync::Arc, time::Instant};
use tokio::sync::{Mutex, watch};
pub type Environment = BTreeMap<String, String>;
pub type McpClient = maho_ext_mcp::transport_sdk::McpClient;
pub type Revalidation = Pin<Box<dyn Future<Output = super::Result<Option<Declaration>>> + Send>>;
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Provenance {
    pub kind: String,
    pub selected_root: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_root: Option<String>,
}
#[derive(Clone)]
pub struct Declaration {
    pub config: Value,
    pub source_path: String,
    pub source_kind: String,
}
#[derive(Clone, Default)]
pub struct SkillMcpClientInfo {
    pub source_path: Option<String>,
    pub source_kind: Option<String>,
    pub provenance: Option<Provenance>,
    pub host_user_trusted: bool,
    pub revalidate_declaration: Option<Arc<dyn Fn() -> Revalidation + Send + Sync>>,
    pub server_name: String,
    pub skill_name: String,
    pub session_id: String,
    pub scope: Option<String>,
    pub directory: Option<String>,
}
pub struct SkillMcpServerContext {
    pub config: Value,
    pub skill_name: String,
}
pub struct ManagedClient {
    pub client: Arc<McpClient>,
    pub skill_name: String,
    pub last_used_at: Instant,
    pub identity: Option<String>,
}
pub type ConnectionResult = super::Result<Arc<McpClient>>;
pub struct PendingConnection {
    pub token: String,
    pub result: watch::Receiver<Option<ConnectionResult>>,
}
#[derive(Default)]
pub struct SkillMcpManagerState {
    pub clients: BTreeMap<String, ManagedClient>,
    pub pending_connections: BTreeMap<String, PendingConnection>,
    pub disconnected_sessions: BTreeMap<String, u64>,
    pub shutdown_generation: u64,
    pub disposed: bool,
}
pub type SharedState = Arc<Mutex<SkillMcpManagerState>>;
