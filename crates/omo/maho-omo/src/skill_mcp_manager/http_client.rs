use super::types::{McpClient, SkillMcpClientInfo};
use serde_json::Value;
use std::sync::{Arc, Mutex};
pub async fn create_http_client(
    info: &SkillMcpClientInfo,
    config: &Value,
) -> super::Result<Arc<McpClient>> {
    let url = config["url"].as_str().ok_or_else(|| {
        format!(
            "MCP server {:?} is configured for HTTP but missing 'url' field.",
            info.server_name
        )
    })?;
    let url = url.parse().map_err(|_| {
        format!(
            "MCP server {:?} has invalid URL: {}",
            info.server_name,
            maho_ext_mcp::log::redact_url(url)
        )
    })?;
    let headers = config
        .get("headers")
        .cloned()
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default();
    let spec = maho_ext_mcp::transport_sdk::McpTransportSpec::Http { url, headers };
    let logger = maho_ext_mcp::log::McpLogger::new(
        &info.server_name,
        &std::env::temp_dir().join("omo-mcp-logs"),
        None,
    )
    .map_err(|e| e.to_string())?;
    let client = McpClient::materialize(&info.server_name, &spec, Arc::new(Mutex::new(logger)))
        .await
        .map_err(|e| e.to_string())?;
    if config.get("oauth").is_some() {
        use maho_ext_mcp::auth::{
            oauth_provider::McpOAuthProvider, oauth_refresh::McpRefreshManager,
            token_store::McpTokenStore,
        };
        let root = super::env_grants_store::resolve_grants_store_path(None)
            .parent()
            .expect("config directory")
            .to_path_buf();
        let url = config["url"].as_str().expect("validated HTTP URL");
        let mut provider = McpOAuthProvider::new(McpTokenStore::new(&root, &info.server_name, url));
        provider.client_id = config["oauth"]["clientId"].as_str().map(str::to_owned);
        provider.scopes = config["oauth"]["scopes"].as_array().map(|scopes| {
            scopes
                .iter()
                .filter_map(|s| s.as_str().map(str::to_owned))
                .collect()
        });
        let http = reqwest::Client::new();
        let refresh = Arc::new(McpRefreshManager::new(Arc::new(provider), http));
        match refresh.ensure_fresh().await {
            Ok(Some(_)) => client.set_auth(refresh).await,
            Ok(None) => {
                if let Err(error) = login(info, config, &root).await {
                    eprintln!("MCP OAuth login fallback: {error}");
                } else {
                    client.set_auth(refresh).await;
                }
            }
            Err(error) => {
                eprintln!("MCP OAuth refresh fallback: {error}");
                if let Err(error) = login(info, config, &root).await {
                    eprintln!("MCP OAuth login fallback: {error}");
                } else {
                    client.set_auth(refresh).await;
                }
            }
        }
    }
    Ok(client)
}
pub(super) async fn login(
    info: &SkillMcpClientInfo,
    config: &Value,
    root: &std::path::Path,
) -> super::Result<()> {
    use maho_ext_mcp::{
        auth::commands_auth::{InteractiveLoginOptions, open_browser, run_interactive_login},
        config_schema::{McpServerConfig, OAuthConfig},
    };
    let oauth: OAuthConfig =
        serde_json::from_value(config["oauth"].clone()).map_err(|e| e.to_string())?;
    let server = McpServerConfig {
        url: config["url"].as_str().map(str::to_owned),
        oauth: Some(oauth),
        ..Default::default()
    };
    let http = reqwest::Client::new();
    run_interactive_login(
        InteractiveLoginOptions {
            name: &info.server_name,
            config: &server,
            agent_dir: root,
            callback_url: None,
            port: None,
            force: true,
            require_https: true,
            client: &http,
        },
        |url| async move {
            open_browser(url.as_str());
            Ok(())
        },
    )
    .await
    .map_err(|e| e.to_string())
}
