use crate::{
    errors::McpError,
    expose::{
        pagination::{McpListPage, McpPaginationResult, collect_all_pages},
        policy::CatalogIdentity,
    },
    transport_sdk::McpClient,
};
use serde_json::{Value, json};
use std::{sync::Arc, time::Duration};
#[derive(Clone)]
pub struct McpToolCatalogEntry {
    pub server: String,
    pub tool: String,
    pub schema: Value,
    /// Modern servers may declare an output schema (raw JSON Schema, possibly boolean).
    pub output_schema: Option<Value>,
    /// The tool came from a modern (2026-07-28) server: its schemas validate as JSON Schema 2020-12.
    pub modern: bool,
    pub description: Option<String>,
    pub annotations: Option<Value>,
    pub request_timeout: Duration,
    pub client: Option<Arc<McpClient>>,
    pub runtime: Option<Arc<McpCatalogRuntime>>,
    pub connection: Option<Arc<crate::connection::ServerConnection>>,
    pub ensure_connected: Option<McpCatalogCallback>,
    pub ensure_fresh: Option<McpCatalogCallback>,
    pub agent_dir: Option<std::path::PathBuf>,
    pub artifacts: Option<Arc<crate::guard::output_guard::McpOutputArtifacts>>,
    pub output_guard: Option<crate::config_schema::OutputGuardSettings>,
    /// The binding-issued request origin (service.ts `#requestOrigin`), absent for unbound callers.
    pub request_origin: Option<crate::request_origin::McpRequestOriginFactory>,
    /// invocation.ts resolver: re-resolves the call against the live session and its permission
    /// fence before each send. Absent for catalogs built outside a session registration.
    pub invocation: Option<Arc<crate::invocation::McpInvocationResolver>>,
}
pub type McpCatalogCallback = Arc<
    dyn Fn() -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), McpError>> + Send>>
        + Send
        + Sync,
>;
pub struct McpCatalogRuntime {
    pub connection: Arc<crate::connection::ServerConnection>,
    pub lifecycle: Arc<crate::idle::McpConnectionLifecycle>,
    pub health: crate::health::McpHealthState,
}
impl CatalogIdentity for McpToolCatalogEntry {
    fn server(&self) -> &str {
        &self.server
    }
    fn tool(&self) -> &str {
        &self.tool
    }
}
pub async fn collect_client_pages(
    client: &McpClient,
    method: &str,
    key: &str,
    timeout: Duration,
) -> Result<McpPaginationResult<Value>, McpError> {
    collect_all_pages(|cursor| async move {
        let params = cursor.map_or_else(|| json!({}), |cursor| json!({"cursor":cursor}));
        let response = client.request(method, params, timeout).await?;
        let next_cursor = response
            .get("nextCursor")
            .filter(|cursor| !cursor.is_null())
            .and_then(Value::as_str)
            .map(str::to_owned);
        Ok(McpListPage {
            items: response.get(key).and_then(Value::as_array).cloned(),
            next_cursor,
            ..Default::default()
        })
    })
    .await
}
/// Lists through `tool_client::list_mcp_tools`, so a modern server pages through the MRTR engine.
pub async fn collect_tool_catalog(
    server: &str,
    client: Arc<McpClient>,
    timeout: Duration,
) -> Result<Vec<McpToolCatalogEntry>, McpError> {
    let listed = collect_all_pages(|cursor| {
        let client = client.clone();
        async move {
            let params = cursor.map_or_else(|| json!({}), |cursor| json!({"cursor":cursor}));
            let response = crate::tool_client::list_mcp_tools(
                &client,
                params,
                timeout,
                &crate::request_origin::McpRequestOptions::default(),
            )
            .await?;
            let next_cursor = response
                .get("nextCursor")
                .filter(|cursor| !cursor.is_null())
                .and_then(Value::as_str)
                .map(str::to_owned);
            Ok::<McpListPage<Value>, McpError>(McpListPage {
                items: response.get("tools").and_then(Value::as_array).cloned(),
                next_cursor,
                ..Default::default()
            })
        }
    })
    .await?;
    let modern = crate::modern::request::is_modern_mcp_client(&client);
    let mut entries = cached_tools_to_catalog_entries(server, &listed.items, client, timeout);
    for entry in &mut entries {
        entry.modern = modern;
    }
    Ok(entries)
}
fn output_schema(tool: &Value) -> Option<Value> {
    tool.get("outputSchema")
        .filter(|schema| schema.is_object() || schema.is_boolean())
        .cloned()
}
/// `mcpRegistrationIdentity`: what one session registered for a server, so a connect-only refresh can tell an
/// unchanged catalog from a stale registration.
pub fn mcp_registration_identity(
    tools: &[McpToolCatalogEntry],
    catalog: Option<&crate::catalog_cache::McpCachedServerCatalog>,
) -> String {
    json!([
        tools
            .iter()
            .map(|entry| json!([
                entry.tool,
                entry.description,
                entry.schema,
                entry.output_schema,
                entry.modern,
                entry.annotations
            ]))
            .collect::<Vec<_>>(),
        catalog.map_or(&[][..], |catalog| catalog.resources.as_slice()),
        catalog.map_or(&[][..], |catalog| catalog.prompts.as_slice()),
    ])
    .to_string()
}
pub fn cached_tools_to_catalog_entries(
    server: &str,
    tools: &[Value],
    client: Arc<McpClient>,
    timeout: Duration,
) -> Vec<McpToolCatalogEntry> {
    tools
        .iter()
        .map(|tool| McpToolCatalogEntry {
            server: server.into(),
            tool: tool
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .into(),
            schema: tool.get("inputSchema").cloned().unwrap_or(Value::Null),
            output_schema: output_schema(tool),
            modern: false,
            description: tool
                .get("description")
                .and_then(Value::as_str)
                .map(str::to_owned),
            annotations: tool.get("annotations").cloned(),
            request_timeout: timeout,
            client: Some(client.clone()),
            runtime: None,
            connection: None,
            ensure_connected: None,
            ensure_fresh: None,
            agent_dir: None,
            artifacts: None,
            output_guard: None,
            request_origin: None,
            invocation: None,
        })
        .collect()
}
pub fn cached_mcp_catalog_entries(
    server: &str,
    catalog: &crate::catalog_cache::McpCachedServerCatalog,
    runtime: Arc<McpCatalogRuntime>,
    timeout: Duration,
    ensure_connected: McpCatalogCallback,
) -> Vec<McpToolCatalogEntry> {
    // A connected client knows its era; otherwise the cache records it.
    let modern = match runtime.connection.client() {
        Ok(client)
            if runtime.connection.state()
                == crate::connection::ServerConnectionState::Connected =>
        {
            crate::modern::request::is_modern_mcp_client(&client)
        }
        _ => catalog.modern,
    };
    catalog
        .tools
        .iter()
        .filter(|tool| {
            !modern || crate::modern::tool_schema::has_supported_modern_tool_schemas(tool)
        })
        .map(|tool| McpToolCatalogEntry {
            server: server.into(),
            tool: tool
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .into(),
            schema: tool.get("inputSchema").cloned().unwrap_or(Value::Null),
            output_schema: output_schema(tool),
            modern,
            description: tool
                .get("description")
                .and_then(Value::as_str)
                .map(str::to_owned),
            annotations: tool.get("annotations").cloned(),
            request_timeout: timeout,
            client: None,
            runtime: Some(runtime.clone()),
            connection: None,
            ensure_connected: Some(ensure_connected.clone()),
            ensure_fresh: None,
            agent_dir: None,
            artifacts: None,
            output_guard: None,
            request_origin: None,
            invocation: None,
        })
        .collect()
}
#[cfg(test)]
mod tests {
    use super::*;
    fn entry(tool: &str, description: Option<&str>) -> McpToolCatalogEntry {
        McpToolCatalogEntry {
            server: "s".into(),
            tool: tool.into(),
            schema: json!({"type":"object"}),
            output_schema: None,
            modern: false,
            description: description.map(str::to_owned),
            annotations: None,
            request_timeout: Duration::from_secs(1),
            client: None,
            runtime: None,
            connection: None,
            ensure_connected: None,
            ensure_fresh: None,
            agent_dir: None,
            artifacts: None,
            output_guard: None,
            request_origin: None,
            invocation: None,
        }
    }
    #[test]
    fn registration_identity_tracks_tools_resources_and_prompts() {
        let tools = vec![entry("a", Some("first"))];
        assert_eq!(
            mcp_registration_identity(&tools, None),
            r#"[[["a","first",{"type":"object"},null,false,null]],[],[]]"#
        );
        assert_ne!(
            mcp_registration_identity(&tools, None),
            mcp_registration_identity(&[entry("a", Some("changed"))], None)
        );
        let catalog = crate::catalog_cache::McpCachedServerCatalog {
            modern: false,
            config_hash: "h".into(),
            credential_identity: None,
            fetched_at: 0.0,
            tools: vec![],
            resources: vec![json!({"uri":"r"})],
            prompts: vec![json!({"name":"p"})],
            instructions: None,
        };
        assert_eq!(
            mcp_registration_identity(&tools, Some(&catalog)),
            r#"[[["a","first",{"type":"object"},null,false,null]],[{"uri":"r"}],[{"name":"p"}]]"#
        );
    }
    #[test]
    fn null_next_cursor_is_not_exposed_to_pagination() {
        let response = json!({"tools":[],"nextCursor":null});
        assert_eq!(
            response
                .get("nextCursor")
                .filter(|cursor| !cursor.is_null())
                .and_then(Value::as_str),
            None
        );
    }
}
