use maho_ext_mcp::auth::{commands_auth::run_loopback_auth, oauth_provider::McpOAuthProvider, token_store::McpTokenStore};

const IDP: &str = "/home/indo/code/senpi/packages/coding-agent/test/mcp/fixtures/oauth-idp.ts";

#[tokio::test]
async fn the_loopback_announcement_retains_the_full_authorization_url() {
    use tokio::{io::{AsyncBufReadExt, BufReader}, process::Command};

    let mut child = Command::new("/usr/bin/node")
        .arg(IDP)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let ready = tokio::time::timeout(std::time::Duration::from_secs(15), lines.next_line())
        .await.unwrap().unwrap().unwrap();
    let ready: serde_json::Value = serde_json::from_str(&ready).unwrap();
    let root = tempfile::tempdir().unwrap();
    let mut provider = McpOAuthProvider::new(McpTokenStore::new(
        root.path(), "fixture", ready["mcpUrl"].as_str().unwrap(),
    ));
    provider.require_https = false;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build().unwrap();
    let callback_client = client.clone();
    let announced = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let captured = announced.clone();
    run_loopback_auth(provider, None, &client, move |authorization| {
        let captured = captured.clone();
        async move {
            let text = authorization.to_string();
            *captured.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = text;
            let redirect = callback_client.get(authorization).send().await?.headers()
                .get("location").unwrap().to_str().unwrap().to_owned();
            callback_client.get(redirect).send().await?.error_for_status()?;
            Ok(())
        }
    }).await.unwrap();
    let url = announced.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
    let parsed = url::Url::parse(&url).expect("announced authorization URL");
    let query = parsed.query_pairs().collect::<Vec<_>>();
    assert!(query.iter().any(|(key, value)| key == "response_type" && value == "code"));
    assert!(query.iter().any(|(key, _)| key == "code_challenge"));
    assert!(query.iter().any(|(key, _)| key == "state"));
    assert!(McpTokenStore::new(root.path(), "fixture", ready["mcpUrl"].as_str().unwrap())
        .read().unwrap().unwrap().access_token.unwrap().starts_with("SENTINEL_AT_"));
    child.kill().await.unwrap();
    child.wait().await.unwrap();
}
