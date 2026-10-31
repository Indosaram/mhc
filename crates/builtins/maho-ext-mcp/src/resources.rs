use crate::{
    config_schema::OutputGuardSettings,
    errors::{McpError, McpErrorKind},
    guard::output_guard::{McpOutputArtifacts, McpOutputGuardOptions, apply_mcp_output_guard},
    transport_sdk::McpClient,
};
use serde_json::{Value, json};
use std::{path::PathBuf, sync::Arc, time::Duration};
#[derive(Clone)]
pub struct McpResourceServer {
    pub server: String,
    pub client: Arc<McpClient>,
    pub agent_dir: PathBuf,
    pub artifacts: Arc<McpOutputArtifacts>,
    pub output_guard: Option<OutputGuardSettings>,
    pub request_timeout: Duration,
    pub resources: Vec<Value>,
    /// The connection behind `client`, for origin-bound reads (upstream `server.connection`).
    pub connection: Option<Arc<crate::connection::ServerConnection>>,
    pub request_origin: Option<crate::request_origin::McpRequestOriginFactory>,
    pub ensure_fresh: Option<crate::catalog::McpCatalogCallback>,
}
pub fn subscribe_mcp_resource_updated(
    client: &McpClient,
    on_change: Arc<dyn Fn() + Send + Sync>,
) -> tokio::task::JoinHandle<()> {
    let mut notifications = client.notifications.subscribe();
    tokio::spawn(async move {
        loop {
            match notifications.recv().await {
                Ok(value) => {
                    if matches!(
                        crate::notification_schemas::parse_notification(&value),
                        Some(crate::notification_schemas::McpNotification::ResourceUpdated { .. })
                    ) {
                        on_change();
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
            }
        }
    })
}
pub fn create_mcp_resource_tools(
    servers: Arc<dyn Fn() -> Vec<McpResourceServer> + Send + Sync>,
) -> Vec<maho_ext_api::ToolDefinition> {
    use maho_ext_api::{ToolContent, ToolDefinition, ToolError, ToolExecutionMode, ToolResult};
    let list_servers = servers.clone();
    let mut list = ToolDefinition::new(
        "mcp_list_resources",
        "List resources exposed by connected MCP servers (URI, name, mime type).",
        json!({"type":"object","properties":{"server":{"type":"string","description":"Restrict to one MCP server name."}},"required":[]}),
        Arc::new(move |call| {
            let servers = list_servers();
            Box::pin(async move {
                let scope = call.params.get("server").and_then(Value::as_str);
                let lines = servers
                    .iter()
                    .filter(|entry| scope.is_none_or(|name| entry.server == name))
                    .flat_map(|entry| {
                        entry.resources.iter().map(|resource| {
                            let uri = resource.get("uri").and_then(Value::as_str).unwrap_or("");
                            let name = resource.get("name").and_then(Value::as_str).unwrap_or(uri);
                            let mime = resource
                                .get("mimeType")
                                .and_then(Value::as_str)
                                .filter(|mime| !mime.is_empty())
                                .map_or_else(String::new, |mime| format!(" ({mime})"));
                            format!("- @mcp:{}/{uri} - {name}{mime}", entry.server)
                        })
                    })
                    .collect::<Vec<_>>();
                Ok(ToolResult {
                    content: vec![ToolContent::text(
                        maho_core::config_reference_redaction::redact_resolved_reference_values(
                            &if lines.is_empty() {
                                "No MCP resources available.".into()
                            } else {
                                lines.join("\n")
                            },
                            "<redacted>",
                        ),
                    )],
                    details: Some(
                        json!({"preview":format!("{} resource(s)",lines.len()),"server":scope.unwrap_or("*"),"tool":"mcp_list_resources"}),
                    ),
                    structured_content: None,
                    is_error: None,
                })
            })
        }),
    );
    list.label = "List MCP resources".into();
    list.execution_mode = Some(ToolExecutionMode::Parallel);
    let mut read = ToolDefinition::new(
        "mcp_read_resource",
        "Read one MCP resource by server name and URI; returns its (guarded) content.",
        json!({"type":"object","properties":{"server":{"type":"string","description":"MCP server name (from mcp_list_resources)."},"uri":{"type":"string","description":"Resource URI to read."}},"required":["server","uri"]}),
        Arc::new(move |call| {
            let servers = servers();
            Box::pin(async move {
                let name = call
                    .params
                    .get("server")
                    .and_then(Value::as_str)
                    .ok_or_else(|| ToolError::Message("Missing MCP resource server".into()))?;
                let uri = call
                    .params
                    .get("uri")
                    .and_then(Value::as_str)
                    .ok_or_else(|| ToolError::Message("Missing MCP resource URI".into()))?;
                let entry = servers
                    .iter()
                    .find(|entry| entry.server == name)
                    .ok_or_else(|| {
                        ToolError::Message(
                            maho_core::config_reference_redaction::redact_resolved_reference_values(
                                &format!("Unknown MCP server '{name}' for resource {uri}."),
                                "<redacted>",
                            ),
                        )
                    })?;
                let text = read_mcp_resource_as_text_with_signal(entry, uri, Some(&call.signal))
                    .await
                    .map_err(|error| ToolError::Message(error.to_string()))?;
                Ok(ToolResult {
                    content: vec![ToolContent::text(text)],
                    details: Some(
                        maho_core::config_reference_redaction::redact_resolved_reference_data(
                            &json!({"preview":uri,"server":name,"tool":"mcp_read_resource"}),
                        ),
                    ),
                    structured_content: None,
                    is_error: None,
                })
            })
        }),
    );
    read.label = "Read MCP resource".into();
    read.execution_mode = Some(ToolExecutionMode::Parallel);
    vec![list, read]
}
pub async fn ensure_mcp_resource_subscriptions(
    client: Arc<McpClient>,
    resources: &[Value],
    timeout: Duration,
) {
    // Modern streams derive their resource URI filter from resources/list results.
    if crate::modern::request::is_modern_mcp_client(&client) {
        return;
    }
    if client
        .server_capabilities
        .read()
        .await
        .pointer("/resources/subscribe")
        != Some(&Value::Bool(true))
    {
        return;
    }
    let mut pending = tokio::task::JoinSet::new();
    for resource in resources {
        if let Some(uri) = resource.get("uri").and_then(Value::as_str) {
            if !client
                .resource_subscriptions
                .lock()
                .await
                .insert(uri.into())
            {
                continue;
            }
            let client = client.clone();
            let uri = uri.to_owned();
            pending.spawn(async move {
                if client
                    .request("resources/subscribe", json!({"uri":uri}), timeout)
                    .await
                    .is_err()
                {
                    client.resource_subscriptions.lock().await.remove(&uri);
                }
            });
        }
    }
    while pending.join_next().await.is_some() {}
}
pub async fn read_mcp_resource_as_text(
    server: &McpResourceServer,
    uri: &str,
) -> Result<String, McpError> {
    read_mcp_resource_as_text_with_signal(server, uri, None).await
}
pub async fn read_mcp_resource_as_text_with_signal(
    server: &McpResourceServer,
    uri: &str,
    signal: Option<&maho_ext_api::AbortSignal>,
) -> Result<String, McpError> {
    let read = async {
        let client = match (&server.request_origin, &server.connection) {
            (Some(_), Some(connection)) => {
                crate::health::ensure_mcp_tool_call_connection(
                    connection,
                    server.ensure_fresh.as_ref(),
                )
                .await?;
                connection.client()?
            }
            (None, Some(connection)) => connection
                .client()
                .unwrap_or_else(|_| server.client.clone()),
            _ => server.client.clone(),
        };
        let options = crate::request_origin::mcp_request_options(
            server.request_origin.as_ref().and_then(|origin| origin()),
            signal.cloned(),
        );
        client
            .request_with_options(
                "resources/read",
                json!({"uri":uri}),
                server.request_timeout,
                &options,
            )
            .await
    };
    let result = read.await.map_err(|error| {
        if crate::modern::errors::as_mcp_mrtr_error(&error).is_some() {
            return error;
        }
        let mut failure = McpError::new(
            McpErrorKind::ToolExec,
            maho_core::config_reference_redaction::redact_resolved_reference_values(
                &format!("Failed to read MCP resource {uri}: {error}"),
                "<redacted>",
            ),
        );
        failure.phase = Some("call".into());
        failure.server_name = Some(server.server.clone());
        failure
    })?;
    let text = flatten_resource_contents(
        result
            .get("contents")
            .and_then(Value::as_array)
            .map_or(&[], Vec::as_slice),
    );
    let guarded = apply_mcp_output_guard(
        &[json!({"type":"text","text":text})],
        McpOutputGuardOptions {
            agent_dir: &server.agent_dir,
            artifacts: Some(&server.artifacts),
            server: &server.server,
            output_guard: server.output_guard.as_ref(),
        },
    );
    Ok(guarded
        .iter()
        .map(|block| block.get("text").and_then(Value::as_str).unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n"))
}
pub fn flatten_resource_contents(contents: &[Value]) -> String {
    contents
        .iter()
        .map(|content| {
            if let Some(text) = content.get("text").and_then(Value::as_str) {
                return text.to_owned();
            }
            let bytes = content
                .get("blob")
                .and_then(Value::as_str)
                .map_or(0, |s| (s.encode_utf16().count() * 3 + 2) / 4);
            format!(
                "[binary resource {} ({}), ~{bytes} bytes]",
                content
                    .get("uri")
                    .and_then(Value::as_str)
                    .unwrap_or("undefined"),
                content
                    .get("mimeType")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}
pub struct McpMentionExpansion {
    pub text: String,
    pub changed: bool,
    pub notices: Vec<String>,
}
pub async fn expand_mcp_resource_mentions(
    text: &str,
    servers: &[McpResourceServer],
) -> Result<McpMentionExpansion, regex::Error> {
    let pattern = regex::Regex::new(r"@mcp:([A-Za-z0-9._-]+)/(\S+)")?;
    let mut expanded = text.to_owned();
    let mut notices = Vec::new();
    for capture in pattern.captures_iter(text) {
        let (Some(mention), Some(name), Some(uri)) =
            (capture.get(0), capture.get(1), capture.get(2))
        else {
            continue;
        };
        let Some(server) = servers.iter().find(|server| server.server == name.as_str()) else {
            notices.push(format!(
                "@mcp mention left as-is: unknown server '{}'.",
                name.as_str()
            ));
            continue;
        };
        match read_mcp_resource_as_text(server, uri.as_str()).await {
            Ok(body) => {
                expanded = expanded.replacen(
                    mention.as_str(),
                    &format!(
                        "<mcp-resource server=\"{}\" uri=\"{}\">\n{body}\n</mcp-resource>",
                        name.as_str(),
                        uri.as_str()
                    ),
                    1,
                )
            }
            Err(error) => notices.push(mcp_mention_failure_notice(&error)),
        }
    }
    let redacted = maho_core::config_reference_redaction::redact_resolved_reference_values(
        &expanded,
        "<redacted>",
    );
    let notices = notices
        .iter()
        .map(|notice| {
            maho_core::config_reference_redaction::redact_resolved_reference_values(
                notice,
                "<redacted>",
            )
        })
        .collect();
    Ok(McpMentionExpansion {
        changed: redacted != text,
        text: redacted,
        notices,
    })
}
/// resources.ts: a mention whose read was canceled, outlived its context, or timed out stays unchanged with a
/// payload-free notice; any other failure names its error.
pub fn mcp_mention_failure_notice(error: &McpError) -> String {
    use crate::modern::errors::McpMrtrErrorCode;
    match crate::modern::errors::as_mcp_mrtr_error(error) {
        Some(mrtr)
            if matches!(
                mrtr.code,
                McpMrtrErrorCode::Cancelled
                    | McpMrtrErrorCode::ContextChanged
                    | McpMrtrErrorCode::Timeout
            ) =>
        {
            "MCP resource request canceled; mention left unchanged.".into()
        }
        _ => format!("@mcp mention left as-is: {error}"),
    }
}
