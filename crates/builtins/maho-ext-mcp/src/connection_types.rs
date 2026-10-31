use crate::errors::McpError;
pub use maho_ext_api::mcp_wire::ServerConnectionState;
#[derive(Debug, Clone)]
pub struct ServerConnectionStateChangedEvent {
    pub server_name: String,
    pub generation: u64,
    pub state: ServerConnectionState,
    pub previous_state: ServerConnectionState,
    pub error: Option<McpError>,
}
/// Upstream `ServerConnectionToolsChangedEvent["cause"]`: `Connect` is raised by every successful
/// connect; `Notification` is anything that reports a change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolsChangedCause {
    Connect,
    Notification,
}
#[derive(Debug, Clone)]
pub struct ServerConnectionToolsChangedEvent {
    pub server_name: String,
    pub generation: u64,
    pub cause: ToolsChangedCause,
}
