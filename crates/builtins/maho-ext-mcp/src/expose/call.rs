//! Port of senpi mcp/expose/call.ts (v2026.10.27-2): one MCP tool call through the connection's lifecycle,
//! session-expiry and failed-send retries, with progress forwarded and failures redacted.
//!
//! DIVERGENCE: upstream resolves an `McpInvocation` (permission dispatch fences, schema re-validation against
//! the latest catalog) before each attempt. The native permission system exposes no dispatch authorizer, so the
//! port validates a modern tool's arguments once in `build_mcp_tool_definitions` and calls the offered entry.
use crate::{catalog::McpToolCatalogEntry, errors::McpError};
use maho_ext_api::{ToolContent, ToolError, ToolResult};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_PROGRESS_TOKEN: AtomicU64 = AtomicU64::new(0);

/// `callMcpTool`: the raw `tools/call` result of `entry`, or a redacted `ToolExecError`. An MRTR failure keeps
/// its fixed, payload-free message.
pub async fn call_mcp_tool(
    entry: &McpToolCatalogEntry,
    args: Value,
    call_id: &str,
    signal: &maho_ext_api::AbortSignal,
    on_update: Option<&(dyn Fn(ToolResult) -> Result<(), ToolError> + Send + Sync)>,
) -> Result<Value, ToolError> {
    let operation = || async {
        if let Some(ensure_fresh) = &entry.ensure_fresh
            && let Err(error) = ensure_fresh().await
        {
            return Err(entry
                .runtime
                .as_ref()
                .map(|runtime| &runtime.connection)
                .or(entry.connection.as_ref())
                .and_then(|connection| {
                    crate::health::mark_mcp_connection_needs_auth(connection, &error)
                })
                .unwrap_or(error));
        }
        let client = if let Some(runtime) = &entry.runtime {
            runtime
                .health
                .ensure_connection(&runtime.connection)
                .await?;
            if let Some(ensure_connected) = &entry.ensure_connected {
                ensure_connected().await?;
            }
            runtime.connection.client()?
        } else {
            if let Some(ensure_connected) = &entry.ensure_connected {
                ensure_connected().await?;
            }
            match &entry.connection {
                Some(connection) => connection.client()?,
                None => entry.client.clone().ok_or_else(|| {
                    McpError::new(
                        crate::errors::McpErrorKind::Connect,
                        "MCP catalog entry has no connection",
                    )
                })?,
            }
        };
        let send = || async {
            let client = if let Some(runtime) = &entry.runtime {
                runtime.connection.client()?
            } else if let Some(connection) = &entry.connection {
                connection.client()?
            } else {
                client.clone()
            };
            let token = format!(
                "native:{}:{}:{}:{}",
                entry.server,
                entry.tool,
                call_id,
                NEXT_PROGRESS_TOKEN.fetch_add(1, Ordering::Relaxed)
            );
            let mut notifications = client.notifications.subscribe();
            let mut notifications_open = true;
            let call_params =
                json!({"name":entry.tool,"arguments":args,"_meta":{"progressToken":token}});
            // `mcpRequestOptions(current.requestOrigin?.(), {signal})`: a fresh binding-issued origin per attempt.
            let mut options = crate::request_origin::mcp_request_options(
                entry.request_origin.as_ref().and_then(|origin| origin()),
                Some(signal.clone()),
            );
            // call.ts `onprogress`: the modern engine reports attempt progress and synthesized task status here.
            let (progress_sender, mut engine_progress) =
                tokio::sync::mpsc::unbounded_channel::<Value>();
            options.on_progress = Some(std::sync::Arc::new(move |progress: Value| {
                let _ = progress_sender.send(progress);
            }));
            let report = |progress: &Value| -> Result<(), McpError> {
                let Some(update) = on_update else {
                    return Ok(());
                };
                let label = format!("{}/{}", entry.server, entry.tool);
                update(ToolResult {
                    content: vec![ToolContent::text(
                        maho_core::config_reference_redaction::redact_resolved_reference_values(
                            &format_progress(&label, progress),
                            "<redacted>",
                        ),
                    )],
                    details: Some(
                        maho_core::config_reference_redaction::redact_resolved_reference_data(
                            &json!({"progress":progress,"server":entry.server,"tool":entry.tool}),
                        ),
                    ),
                    structured_content: None,
                    is_error: None,
                })
                .map_err(|error| {
                    McpError::new(crate::errors::McpErrorKind::ToolExec, error.to_string())
                })
            };
            // tool-client.ts: a modern call validates structured output against the declared output schema.
            let request = async {
                if crate::modern::request::is_modern_mcp_client(&client) {
                    crate::tool_client::call_mcp_tool_request(
                        &client,
                        call_params,
                        entry.output_schema.as_ref(),
                        entry.request_timeout,
                        &options,
                    )
                    .await
                    .map(Value::Object)
                } else {
                    client
                        .request_with_options(
                            "tools/call",
                            call_params,
                            entry.request_timeout,
                            &options,
                        )
                        .await
                }
            };
            tokio::pin!(request);
            let result = loop {
                tokio::select! {
                    biased;
                    notification = notifications.recv(), if notifications_open => {
                        let value = match notification {
                            Ok(value) => value,
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                                notifications_open = false;
                                continue;
                            }
                        };
                        let progress = value.get("params").unwrap_or(&Value::Null);
                        if value.get("method").and_then(Value::as_str) == Some("notifications/progress")
                            && progress.get("progressToken") == Some(&json!(token))
                        {
                            report(progress)?;
                        }
                    }
                    Some(progress) = engine_progress.recv() => report(&progress)?,
                    result = &mut request => {
                        signal.check().map_err(|error| McpError::new(crate::errors::McpErrorKind::ToolExec, error.to_string()))?;
                        break result?;
                    }
                }
            };
            Ok::<_, McpError>(result)
        };
        if let Some(runtime) = &entry.runtime {
            crate::health::with_mcp_retriable_failed_send_retry(&runtime.connection, send).await
        } else {
            send().await
        }
    };
    let result = if let Some(runtime) = &entry.runtime {
        runtime
            .lifecycle
            .run_call(crate::health::with_mcp_session_expiry_retry(
                &runtime.connection,
                operation,
            ))
            .await
    } else {
        operation().await
    };
    result.map_err(|error| {
        ToolError::Message(
            if crate::modern::errors::as_mcp_mrtr_error(&error).is_some() {
                error.message.clone()
            } else {
                maho_core::config_reference_redaction::redact_resolved_reference_values(
                    &format!("ToolExecError: {error}"),
                    "<redacted>",
                )
            },
        )
    })
}

/// `formatProgress`: `<label> progress <n>[/<total>][ <message>]`, trimmed.
pub fn format_progress(label: &str, progress: &Value) -> String {
    let total = progress
        .get("total")
        .map_or_else(String::new, |total| format!("/{total}"));
    let message = progress
        .get("message")
        .and_then(Value::as_str)
        .map_or_else(String::new, |message| format!(" {message}"));
    format!(
        "{label} progress {}{total}{message}",
        progress.get("progress").unwrap_or(&Value::Null)
    )
    .trim()
    .to_owned()
}
