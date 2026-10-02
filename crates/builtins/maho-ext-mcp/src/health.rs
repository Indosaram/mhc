use std::{sync::Arc,time::Duration};
use serde_json::{Value,json};
use crate::{connection::{ServerConnection,ServerConnectionState},errors::{McpError,McpErrorKind,is_mcp_session_expired_error,is_retriable_mcp_error}};
pub const MCP_PING_STALE_MS:u64=30000;
pub const MCP_PING_TIMEOUT_MS:u64=2000;
pub fn mark_mcp_connection_needs_auth(connection:&ServerConnection,cause:&McpError)->Option<McpError> {
    if !crate::needs_auth::is_mcp_needs_auth_error(cause){return None;}
    let mut error=McpError::new(McpErrorKind::Auth,format!("MCP server {} needs OAuth. Run senpi interactive, then /mcp auth-start {} and /mcp auth-complete {} <redirect-url>.",connection.server_name,connection.server_name,connection.server_name));
    error.phase=Some("auth".into());error.server_name=Some(connection.server_name.clone());error.cause=Some(Box::new(error_value(cause)));
    connection.mark_failure(ServerConnectionState::NeedsAuth,Some(error.clone()));Some(error)
}
fn error_value(error:&McpError)->Value {json!({"message":error.message,"retriable":error.retriable,"cause":error.cause})}
fn expired(connection:&ServerConnection,cause:McpError,retriable:bool)->McpError {
    let suffix=if retriable {"reinitializing once".into()}else{format!("reinitialize retry also expired; run /mcp reconnect {}",connection.server_name)};
    let mut error=McpError::new(McpErrorKind::SessionExpired,format!("MCP server {} session expired; {suffix}",connection.server_name));
    error.phase=Some("session".into());error.server_name=Some(connection.server_name.clone());error.retriable=retriable;error.cause=Some(Box::new(error_value(&cause)));error
}
fn failed_send(connection:&ServerConnection,cause:&McpError,retriable:bool)->McpError {
    let suffix=if retriable {"reconnecting once before retry".into()}else{format!("post-reconnect retry also failed; run /mcp reconnect {}",connection.server_name)};
    let mut error=McpError::new(McpErrorKind::Connect,format!("MCP server {} failed to send tool call; {suffix}",connection.server_name));
    error.phase=Some("call".into());error.server_name=Some(connection.server_name.clone());error.retriable=retriable;error.cause=Some(Box::new(error_value(cause)));error
}
pub async fn with_mcp_session_expiry_retry<T,F,Fut>(connection:&Arc<ServerConnection>,mut operation:F)->Result<T,McpError>
where F:FnMut()->Fut,Fut:std::future::Future<Output=Result<T,McpError>> {
    match operation().await {
        Ok(value)=>Ok(value),
        Err(error) if is_mcp_session_expired_error(&error_value(&error))=>{
            connection.mark_failure(ServerConnectionState::Degraded,Some(expired(connection,error,true)));
            connection.renew().await?;
            match operation().await {
                Err(error) if is_mcp_session_expired_error(&error_value(&error))=>{
                    let error=expired(connection,error,false);connection.mark_failure(ServerConnectionState::Suspended,Some(error.clone()));Err(error)
                }
                result=>result,
            }
        }
        Err(error)=>Err(error),
    }
}
pub async fn with_mcp_retriable_failed_send_retry<T,F,Fut>(connection:&Arc<ServerConnection>,mut operation:F)->Result<T,McpError>
where F:FnMut()->Fut,Fut:std::future::Future<Output=Result<T,McpError>> {
    match operation().await {
        Ok(value)=>Ok(value),
        Err(error) if is_retriable_mcp_error(&error_value(&error)) && !is_mcp_session_expired_error(&error_value(&error))=>{
            connection.mark_failure(ServerConnectionState::Degraded,Some(failed_send(connection,&error,true)));
            connection.renew().await?;
            let result=operation().await;
            if let Err(error)=&result && is_retriable_mcp_error(&error_value(error)) && !is_mcp_session_expired_error(&error_value(error)) {
                connection.mark_failure(ServerConnectionState::Degraded,Some(failed_send(connection,error,false)));
            }
            result
        }
        Err(error)=>Err(error),
    }
}
#[derive(Default)]
pub struct McpHealthState {validation:tokio::sync::Mutex<Option<tokio::time::Instant>>}
impl McpHealthState {
    pub async fn ensure_connection(&self,connection:&Arc<ServerConnection>)->Result<(),McpError> {
        let mut successful=self.validation.lock().await;
        match connection.state() {
            ServerConnectionState::Idle|ServerConnectionState::Connecting=>{connection.connect().await?;}
            ServerConnectionState::NeedsAuth=>return Err(McpError::new(McpErrorKind::Auth,format!("MCP server {} needs OAuth. Run senpi interactive, then /mcp auth-start {} and /mcp auth-complete {} <redirect-url>.",connection.server_name,connection.server_name,connection.server_name))),
            ServerConnectionState::Connected=>(),
            ServerConnectionState::Disabled|ServerConnectionState::Degraded|ServerConnectionState::Suspended|ServerConnectionState::NeedsClientRegistration=>{
                connection.renew().await?;*successful=Some(tokio::time::Instant::now());return Ok(());
            }
        }
        if successful.is_some_and(|at|at.elapsed()<=Duration::from_millis(MCP_PING_STALE_MS)){return Ok(());}
        if let Err(error)=connection.client()?.request("ping",json!({}),Duration::from_millis(MCP_PING_TIMEOUT_MS)).await {
            connection.mark_failure(ServerConnectionState::Degraded,Some(error));connection.renew().await?;
        }
        *successful=Some(tokio::time::Instant::now());Ok(())
    }
}
