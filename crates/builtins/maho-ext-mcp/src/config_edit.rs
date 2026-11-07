use crate::{
    config::{McpConfigValidationError, validate_raw},
    config_schema::{McpServerConfig, RawConfig, Transport},
};
use std::{
    fs,
    path::{Path, PathBuf},
};
pub fn get_global_mcp_config_path(agent_dir: &Path) -> PathBuf {
    agent_dir.join("mcp.json")
}
fn read_global(path: &Path) -> Result<RawConfig, McpConfigValidationError> {
    if !path.exists() {
        return Ok(RawConfig::default());
    }
    let data = fs::read_to_string(path).map_err(|e| McpConfigValidationError(e.to_string()))?;
    validate_raw(serde_json::from_str(&data).map_err(|e| McpConfigValidationError(e.to_string()))?)
}
fn write_validated(path: &Path, config: &RawConfig) -> Result<(), McpConfigValidationError> {
    validate_raw(
        serde_json::to_value(config).map_err(|e| McpConfigValidationError(e.to_string()))?,
    )?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| McpConfigValidationError(e.to_string()))?;
    }
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut file = opts
        .open(path)
        .map_err(|e| McpConfigValidationError(e.to_string()))?;
    use std::io::Write;
    writeln!(
        file,
        "{}",
        serde_json::to_string_pretty(config)
            .map_err(|e| McpConfigValidationError(e.to_string()))?
    )
    .map_err(|e| McpConfigValidationError(e.to_string()))
}
pub fn add_global_mcp_server(
    agent_dir: &Path,
    name: &str,
    server: &McpServerConfig,
) -> Result<PathBuf, McpConfigValidationError> {
    let path = get_global_mcp_config_path(agent_dir);
    let mut config = read_global(&path)?;
    let stripped = match server.transport {
        Some(Transport::Http) => McpServerConfig {
            transport: Some(Transport::Http),
            url: Some(server.url.clone().unwrap_or_default()),
            ..Default::default()
        },
        _ => McpServerConfig {
            transport: Some(Transport::Stdio),
            command: Some(server.command.clone().unwrap_or_default()),
            args: server.args.clone(),
            ..Default::default()
        },
    };
    config
        .mcp_servers
        .get_or_insert_with(Default::default)
        .insert(name.into(), stripped);
    write_validated(&path, &config)?;
    Ok(path)
}
pub fn set_global_mcp_server_enabled(
    agent_dir: &Path,
    name: &str,
    enabled: bool,
) -> Result<bool, McpConfigValidationError> {
    let path = get_global_mcp_config_path(agent_dir);
    let mut config = read_global(&path)?;
    let Some(server) = config.mcp_servers.as_mut().and_then(|m| m.get_mut(name)) else {
        return Ok(false);
    };
    server.enabled = Some(enabled);
    write_validated(&path, &config)?;
    Ok(true)
}
/// Upstream `updateMcpServerConfig`: patch `enabled`/`exposure` of one server in a resolved global or
/// project source, retaining placeholders, reference objects and unrelated fields. `Ok(false)` when the
/// server is no longer in that file.
pub fn update_mcp_server_config(
    path: &Path,
    name: &str,
    enabled: Option<bool>,
    exposure: Option<crate::config_schema::Exposure>,
) -> Result<bool, McpConfigValidationError> {
    let error = |e: &dyn std::fmt::Display| McpConfigValidationError(e.to_string());
    let mut raw = if path.exists() {
        let data = fs::read_to_string(path).map_err(|e| error(&e))?;
        serde_json::from_str::<serde_json::Value>(&data).map_err(|e| error(&e))?
    } else {
        serde_json::json!({})
    };
    validate_raw(raw.clone())?;
    let Some(server) = raw
        .get_mut("mcpServers")
        .and_then(|servers| servers.get_mut(name))
        .and_then(serde_json::Value::as_object_mut)
    else {
        return Ok(false);
    };
    if let Some(enabled) = enabled {
        server.insert("enabled".into(), serde_json::Value::Bool(enabled));
    }
    if let Some(exposure) = exposure {
        server.insert(
            "exposure".into(),
            serde_json::to_value(exposure).map_err(|e| error(&e))?,
        );
    }
    validate_raw(raw.clone())?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| error(&e))?;
    }
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut file = opts.open(path).map_err(|e| error(&e))?;
    use std::io::Write;
    writeln!(
        file,
        "{}",
        serde_json::to_string_pretty(&raw).map_err(|e| error(&e))?
    )
    .map_err(|e| error(&e))?;
    Ok(true)
}
