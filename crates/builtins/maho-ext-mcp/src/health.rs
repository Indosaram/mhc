use crate::{
    connection::{ServerConnection, ServerConnectionState},
    errors::{McpError, McpErrorKind, is_mcp_session_expired_error, is_retriable_mcp_error},
};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
pub const MCP_PING_STALE_MS: u64 = 30000;
pub const MCP_PING_TIMEOUT_MS: u64 = 2000;
/// Upstream `headlessAuthError`.
fn headless_auth_error(connection: &ServerConnection, cause: Option<&McpError>) -> McpError {
    let mut error = McpError::new(
        McpErrorKind::Auth,
        format!(
            "MCP server {} needs OAuth. Run senpi interactive, then /mcp auth-start {} and /mcp auth-complete {} <redirect-url>.",
            connection.server_name, connection.server_name, connection.server_name
        ),
    );
    error.phase = Some("auth".into());
    error.server_name = Some(connection.server_name.clone());
    error.cause = cause.map(|cause| Box::new(error_value(cause)));
    error
}
/// A needs-auth failure from connect or renew surfaces as the headless sign-in instruction.
fn headless_on_needs_auth(connection: &ServerConnection, error: McpError) -> McpError {
    if crate::needs_auth::is_mcp_needs_auth_error(&error) {
        headless_auth_error(connection, Some(&error))
    } else {
        error
    }
}
pub fn mark_mcp_connection_needs_auth(
    connection: &ServerConnection,
    cause: &McpError,
) -> Option<McpError> {
    if !crate::needs_auth::is_mcp_needs_auth_error(cause) {
        return None;
    }
    let error = headless_auth_error(connection, Some(cause));
    connection.mark_failure(ServerConnectionState::NeedsAuth, Some(error.clone()));
    Some(error)
}
fn error_value(error: &McpError) -> Value {
    json!({"message":error.message,"retriable":error.retriable,"cause":error.cause})
}
fn expired(connection: &ServerConnection, cause: McpError, retriable: bool) -> McpError {
    let suffix = if retriable {
        "reinitializing once".into()
    } else {
        format!(
            "reinitialize retry also expired; run /mcp reconnect {}",
            connection.server_name
        )
    };
    let mut error = McpError::new(
        McpErrorKind::SessionExpired,
        format!(
            "MCP server {} session expired; {suffix}",
            connection.server_name
        ),
    );
    error.phase = Some("session".into());
    error.server_name = Some(connection.server_name.clone());
    error.retriable = retriable;
    error.cause = Some(Box::new(error_value(&cause)));
    error
}
fn failed_send(connection: &ServerConnection, cause: &McpError, retriable: bool) -> McpError {
    let suffix = if retriable {
        "reconnecting once before retry".into()
    } else {
        format!(
            "post-reconnect retry also failed; run /mcp reconnect {}",
            connection.server_name
        )
    };
    let mut error = McpError::new(
        McpErrorKind::Connect,
        format!(
            "MCP server {} failed to send tool call; {suffix}",
            connection.server_name
        ),
    );
    error.phase = Some("call".into());
    error.server_name = Some(connection.server_name.clone());
    error.retriable = retriable;
    error.cause = Some(Box::new(error_value(cause)));
    error
}
pub async fn with_mcp_session_expiry_retry<T, F, Fut>(
    connection: &Arc<ServerConnection>,
    mut operation: F,
) -> Result<T, McpError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, McpError>>,
{
    match operation().await {
        Ok(value) => Ok(value),
        // A local MRTR failure is final: never reconnect and replay it.
        Err(error) if crate::modern::errors::as_mcp_mrtr_error(&error).is_some() => Err(error),
        Err(error) if is_mcp_session_expired_error(&error_value(&error)) => {
            connection.mark_failure(
                ServerConnectionState::Degraded,
                Some(expired(connection, error, true)),
            );
            connection.renew().await?;
            match operation().await {
                Err(error) if is_mcp_session_expired_error(&error_value(&error)) => {
                    let error = expired(connection, error, false);
                    connection.mark_failure(ServerConnectionState::Suspended, Some(error.clone()));
                    Err(error)
                }
                result => result,
            }
        }
        Err(error) => Err(error),
    }
}
pub async fn with_mcp_retriable_failed_send_retry<T, F, Fut>(
    connection: &Arc<ServerConnection>,
    mut operation: F,
) -> Result<T, McpError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, McpError>>,
{
    match operation().await {
        Ok(value) => Ok(value),
        Err(error) if crate::modern::errors::as_mcp_mrtr_error(&error).is_some() => Err(error),
        Err(error)
            if is_retriable_mcp_error(&error_value(&error))
                && !is_mcp_session_expired_error(&error_value(&error)) =>
        {
            connection.mark_failure(
                ServerConnectionState::Degraded,
                Some(failed_send(connection, &error, true)),
            );
            connection.renew().await?;
            let result = operation().await;
            if let Err(error) = &result
                && is_retriable_mcp_error(&error_value(error))
                && !is_mcp_session_expired_error(&error_value(error))
            {
                connection.mark_failure(
                    ServerConnectionState::Degraded,
                    Some(failed_send(connection, error, false)),
                );
            }
            result
        }
        Err(error) => Err(error),
    }
}
/// Upstream `ensureMcpToolCallConnection` for origin-bound prompt/resource requests: fresh auth first, then a
/// lazy connect, so the request's origin is taken against the client it will actually use.
pub async fn ensure_mcp_tool_call_connection(
    connection: &Arc<ServerConnection>,
    ensure_fresh: Option<&crate::catalog::McpCatalogCallback>,
) -> Result<(), McpError> {
    if let Some(ensure_fresh) = ensure_fresh
        && let Err(error) = ensure_fresh().await
    {
        return Err(mark_mcp_connection_needs_auth(connection, &error).unwrap_or(error));
    }
    if matches!(
        connection.state(),
        ServerConnectionState::Idle | ServerConnectionState::Connecting
    ) {
        connection
            .connect()
            .await
            .map_err(|error| headless_on_needs_auth(connection, error))?;
    }
    Ok(())
}
#[derive(Default)]
pub struct McpHealthState {
    validation: tokio::sync::Mutex<Option<tokio::time::Instant>>,
}
impl McpHealthState {
    pub async fn ensure_connection(
        &self,
        connection: &Arc<ServerConnection>,
    ) -> Result<(), McpError> {
        let mut successful = self.validation.lock().await;
        match connection.state() {
            ServerConnectionState::Idle | ServerConnectionState::Connecting => {
                connection
                    .connect()
                    .await
                    .map_err(|error| headless_on_needs_auth(connection, error))?;
            }
            ServerConnectionState::NeedsAuth => return Err(headless_auth_error(connection, None)),
            ServerConnectionState::Connected => (),
            ServerConnectionState::Disabled
            | ServerConnectionState::Degraded
            | ServerConnectionState::Suspended
            | ServerConnectionState::NeedsClientRegistration => {
                connection
                    .renew()
                    .await
                    .map_err(|error| headless_on_needs_auth(connection, error))?;
                *successful = Some(tokio::time::Instant::now());
                return Ok(());
            }
        }
        if successful.is_some_and(|at| at.elapsed() <= Duration::from_millis(MCP_PING_STALE_MS)) {
            return Ok(());
        }
        if let Err(error) = connection
            .client()?
            .request(
                "ping",
                json!({}),
                Duration::from_millis(MCP_PING_TIMEOUT_MS),
            )
            .await
        {
            connection.mark_failure(ServerConnectionState::Degraded, Some(error));
            connection
                .renew()
                .await
                .map_err(|error| headless_on_needs_auth(connection, error))?;
        }
        *successful = Some(tokio::time::Instant::now());
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn needs_auth_state_fails_with_the_headless_sign_in_error() {
        let root = tempfile::tempdir().unwrap();
        let logger = Arc::new(std::sync::Mutex::new(
            crate::log::McpLogger::new("remote", root.path(), None).unwrap(),
        ));
        let connection = ServerConnection::new(
            "remote",
            crate::config_schema::McpServerConfig {
                enabled: Some(true),
                ..Default::default()
            },
            None,
            logger,
        );
        connection.mark_failure(ServerConnectionState::NeedsAuth, None);
        let error = McpHealthState::default()
            .ensure_connection(&connection)
            .await
            .unwrap_err();
        assert_eq!(error.kind, McpErrorKind::Auth);
        assert_eq!(error.phase.as_deref(), Some("auth"));
        assert_eq!(error.server_name.as_deref(), Some("remote"));
        assert_eq!(
            error.message,
            "MCP server remote needs OAuth. Run senpi interactive, then /mcp auth-start remote and /mcp auth-complete remote <redirect-url>."
        );
    }
}
