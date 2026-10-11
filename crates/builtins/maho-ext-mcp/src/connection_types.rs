use crate::errors::McpError;
pub use maho_ext_api::mcp_wire::ServerConnectionState;
#[derive(Debug,Clone)]
pub struct ServerConnectionStateChangedEvent {pub server_name:String,pub generation:u64,pub state:ServerConnectionState,pub previous_state:ServerConnectionState,pub error:Option<McpError>}
#[derive(Debug,Clone)]
pub struct ServerConnectionToolsChangedEvent {pub server_name:String,pub generation:u64}
