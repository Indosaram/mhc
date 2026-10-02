use crate::{config_schema::ResolvedMcpServer,connection::{ServerConnection,ServerConnectionState},service_types::*};

pub fn build_mcp_server_snapshot(name:&str,server:Option<&ResolvedMcpServer>,connection:Option<&ServerConnection>,entry:Option<&McpConnectionEntry>,now:f64)->McpServerSnapshot {
    let lifecycle_state=match connection {
        Some(connection) if connection.state()==ServerConnectionState::Idle && connection.generation()==0 && entry.is_some_and(|entry|entry.cached_catalog.is_some())=>McpLifecycleState::Other(McpUnspawnedState::Cached),
        Some(connection)=>McpLifecycleState::Connection(connection.state()),
        None=>McpLifecycleState::Other(McpUnspawnedState::NotSpawned),
    };
    McpServerSnapshot {
        name:name.into(),config_state:server.map_or(McpSnapshotConfigState::Removed,|server|server.state.into()),
        config_hash:server.and_then(|server|server.config_hash.clone()),source:server.map(|server|server.source),source_path:server.map(|server|server.source_path.clone()),
        lifecycle_state,generation:connection.map(ServerConnection::generation),pid:connection.and_then(ServerConnection::get_root_pid),
        last_error:connection.and_then(ServerConnection::last_error).map(|error|error.message),uptime_ms:entry.map(|entry|now-entry.created_at_ms),
        counters:entry.map_or_else(McpServerCounters::default,|entry|entry.counters.clone()),
    }
}
