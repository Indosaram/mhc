use super::{
    project_env_policy::create_clean_mcp_environment,
    types::{Environment, McpClient, SkillMcpClientInfo},
};
use serde_json::Value;
use std::sync::{Arc, Mutex};
pub async fn create_stdio_client(
    info: &SkillMcpClientInfo,
    config: &Value,
    environment: Option<&Environment>,
) -> super::Result<Arc<McpClient>> {
    let command = config["command"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            format!(
                "MCP server {:?} is configured for stdio but missing 'command' field.",
                info.server_name
            )
        })?;
    let args = config["args"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    let declared = config
        .get("env")
        .cloned()
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default();
    let env = environment
        .cloned()
        .unwrap_or_else(|| create_clean_mcp_environment(&declared, &std::env::vars().collect()));
    let spec = maho_ext_mcp::transport_sdk::McpTransportSpec::Stdio {
        command: command.into(),
        args,
        cwd: info.directory.clone(),
        env,
    };
    let logger = maho_ext_mcp::log::McpLogger::new(
        &info.server_name,
        &std::env::temp_dir().join("omo-mcp-logs"),
        None,
    )
    .map_err(|e| e.to_string())?;
    McpClient::materialize_stdio_with_policy(
        &info.server_name,
        &spec,
        Arc::new(Mutex::new(logger)),
        true,
    )
    .await
    .map_err(|e| e.to_string())
}
