//! Port of senpi mcp/modern/dispatch.ts: dual-era materialization.
use super::{
    era::{EraOutcome, ProbeHttpRequest, probe_http_era},
    http_fetch::modern_http_fetch,
    stdio_probe::{StdioProbeResult, probe_stdio_transport},
    transport::ModernHttpTransport,
};
use crate::{
    errors::{McpError, McpErrorKind},
    log::McpLogger,
    transport_sdk::{McpClient, McpTransportSpec, mcp_client_info, mcp_probe_options},
};
use serde_json::json;
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

/// What the connection factory hands dispatch (upstream `MaterializeMcpTransportOptions`).
pub struct NegotiationInputs<'a> {
    pub spec: &'a McpTransportSpec,
    pub logger: Arc<Mutex<McpLogger>>,
    pub auth: Option<Arc<crate::auth::oauth_refresh::McpRefreshManager>>,
    pub ui: Option<Arc<dyn maho_ext_api::ExtensionUi>>,
    /// The connection's readiness provider (a pooled connection's lease aggregate).
    pub readiness: Option<crate::elicitation::McpFormReadiness>,
    pub elicitation_enabled: bool,
}

fn connect_error(server: &str, message: String, cause: Option<String>) -> McpError {
    let mut error = McpError::new(McpErrorKind::Connect, message);
    error.phase = Some("connect".into());
    error.server_name = Some(server.into());
    error.cause = cause.map(|cause| Box::new(json!({"message": cause})));
    error
}

fn log_version(
    logger: &Arc<Mutex<McpLogger>>,
    server: &str,
    modern: Option<&str>,
    sdk: Option<&str>,
) {
    let era = if modern.is_some() { "modern" } else { "legacy" };
    let version = modern.or(sdk).unwrap_or("unknown");
    let _ = logger
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .log(
            "info",
            &format!("MCP server {server} negotiated: era={era} version={version}"),
            None,
            None,
        );
}

/// HTTP: probe first and build a modern client with no legacy traffic, else fall back. Stdio: spawn, probe the
/// started child, and restart it for the legacy handshake only when the probe left it unusable.
pub async fn materialize_negotiated_transport<F, Fut>(
    inputs: NegotiationInputs<'_>,
    server_name: &str,
    timeout: Duration,
    materialize: F,
) -> Result<Arc<McpClient>, McpError>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<Arc<McpClient>, McpError>>,
{
    let options = mcp_probe_options();
    if let McpTransportSpec::Http { url, headers } = inputs.spec {
        let fetch = |_url: url::Url, request: ProbeHttpRequest| {
            modern_http_fetch(inputs.spec, inputs.auth.as_ref(), request, timeout, true)
        };
        let outcome = probe_http_era(url, fetch, &options).await?;
        match outcome {
            EraOutcome::Modern {
                version,
                discovery,
                error,
                ..
            } => {
                let Some(discovery) = discovery else {
                    let detail = error.map_or_else(String::new, |error| error.message);
                    return Err(connect_error(
                        server_name,
                        format!("MCP server {server_name} modern discovery failed: {detail}"),
                        None,
                    ));
                };
                let client = materialize().await?;
                super::client::create_mcp_request_client(
                    &client,
                    inputs.ui.as_ref(),
                    inputs.readiness.as_ref(),
                    inputs.elicitation_enabled,
                );
                let warn_logger = inputs.logger.clone();
                let mut configured = headers.clone();
                super::http_fetch::strip_protocol_headers(&mut configured);
                let transport = ModernHttpTransport::new(
                    server_name,
                    url.clone(),
                    configured,
                    mcp_client_info(),
                    json!({}),
                    version.clone(),
                    inputs.auth.clone(),
                    Arc::new(move |message: &str| {
                        let _ = warn_logger
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .log("warning", message, None, None);
                    }),
                )?;
                client.speak_modern(&discovery, &version, Some(transport))?;
                log_version(&inputs.logger, server_name, Some(&version), None);
                return Ok(client);
            }
            EraOutcome::Indeterminate {
                reason,
                action,
                modern_era,
                cause,
                ..
            } if modern_era || reason == "invalid-discovery" => {
                return Err(connect_error(
                    server_name,
                    format!("MCP server {server_name}: {reason}. {action}"),
                    cause,
                ));
            }
            // Unknown endpoints (including legacy session-required 404s and auth challenges) retain legacy diagnostics.
            _ => {}
        }
        let client = materialize().await?;
        super::client::create_mcp_request_client(
            &client,
            inputs.ui.as_ref(),
            inputs.readiness.as_ref(),
            inputs.elicitation_enabled,
        );
        client
            .protocol
            .legacy
            .store(true, std::sync::atomic::Ordering::Release);
        return Ok(client);
    }
    let client = materialize().await?;
    super::client::create_mcp_request_client(
        &client,
        inputs.ui.as_ref(),
        inputs.readiness.as_ref(),
        inputs.elicitation_enabled,
    );
    // A modern stdio child is detected when it starts, before its local initialize answer sets the version.
    match probe_stdio_transport(
        &client,
        &options,
        &inputs.logger,
        timeout,
        server_name,
        None,
    )
    .await?
    {
        StdioProbeResult::Modern(version) => {
            client.protocol.resolve_stdio_diagnostics(true);
            log_version(&inputs.logger, server_name, Some(&version), None);
            Ok(client)
        }
        StdioProbeResult::Legacy { silent } => {
            // stdio-probe.ts: a silent child is classified by its initialize reply (late discovery may still
            // upgrade it); only a clean non-modern answer is legacy now.
            if !silent {
                client
                    .protocol
                    .legacy
                    .store(true, std::sync::atomic::Ordering::Release);
                client.protocol.resolve_stdio_diagnostics(false);
            }
            Ok(client)
        }
        StdioProbeResult::Restart => {
            // Kill the probed tree outright: a silent child may ignore a graceful stdin close for seconds.
            if let Some(pid) = client.root_pid {
                crate::process_tree::reap_process_tree(
                    pid,
                    Duration::from_millis(200),
                    Duration::from_millis(200),
                )
                .await;
            }
            let _ = client.close().await;
            let fresh = materialize().await?;
            super::client::create_mcp_request_client(
                &fresh,
                inputs.ui.as_ref(),
                inputs.readiness.as_ref(),
                inputs.elicitation_enabled,
            );
            fresh
                .protocol
                .legacy
                .store(true, std::sync::atomic::Ordering::Release);
            fresh.protocol.resolve_stdio_diagnostics(false);
            Ok(fresh)
        }
    }
}

/// Logged after `initialize` for legacy connections (upstream `logVersion` on `setProtocolVersion`).
pub fn log_negotiated_version(
    client: &McpClient,
    logger: &Arc<Mutex<McpLogger>>,
    server_name: &str,
) {
    if client.modern_route().is_some() {
        return;
    }
    log_version(
        logger,
        server_name,
        None,
        client.negotiated_version().as_deref(),
    );
}
