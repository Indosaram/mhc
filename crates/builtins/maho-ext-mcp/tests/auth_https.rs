use axum::{Router, routing::get};
use maho_ext_mcp::auth::{
    oauth::{OAuthServerInfo, discover, validate_info_https},
    oauth_provider::McpOAuthProvider,
    token_store::McpTokenStore,
};
use serde_json::{Value, json};

fn info(issuer: &str, metadata: Value) -> OAuthServerInfo {
    OAuthServerInfo { authorization_server_url: issuer.into(), authorization_server_metadata: metadata, resource_metadata: Value::Null }
}

fn good() -> Value {
    json!({
        "authorization_endpoint": "https://idp.test/authorize",
        "token_endpoint": "https://idp.test/token",
        "registration_endpoint": "https://idp.test/register",
    })
}

fn rejection(server: &str, info: &OAuthServerInfo) -> String {
    validate_info_https(server, info).expect_err("expected an error").to_string()
}

#[test]
fn require_https_defaults_to_true() {
    let root = tempfile::tempdir().expect("test fixture");
    assert!(McpOAuthProvider::new(McpTokenStore::new(root.path(), "https", "https://x.test")).require_https);
}

#[test]
fn every_label_is_reported_for_non_https_urls() {
    assert!(validate_info_https("https://rs.test/mcp", &info("https://idp.test", good())).is_ok());
    assert_eq!(rejection("http://rs.test/mcp", &info("https://idp.test", good())), "Resource server URL must use https");
    assert_eq!(rejection("https://rs.test/mcp", &info("http://idp.test", good())), "Authorization server URL must use https");
    for field in ["authorization_endpoint", "token_endpoint", "registration_endpoint"] {
        let mut metadata = good();
        metadata[field] = json!("http://idp.test/x");
        assert_eq!(rejection("https://rs.test/mcp", &info("https://idp.test", metadata)), format!("{field} must use https"));
    }
}

#[tokio::test]
async fn strict_discovery_rejects_http_server_url_and_loopback_http_works_when_off() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("test fixture");
    let base = format!("http://{}", listener.local_addr().expect("test fixture"));
    let metadata = json!({"authorization_endpoint": format!("{base}/authorize"), "token_endpoint": format!("{base}/token")});
    let app = Router::new().route("/.well-known/oauth-authorization-server", get(move || {
        let metadata = metadata.clone();
        async move { axum::Json(metadata) }
    }));
    let task = tokio::spawn(async move { axum::serve(listener, app).await.expect("test fixture") });
    let root = tempfile::tempdir().expect("test fixture");
    let client = reqwest::Client::new();
    let url = format!("{base}/mcp");

    let strict = McpOAuthProvider::new(McpTokenStore::new(root.path(), "https", &url));
    assert_eq!(discover(&strict, &client).await.expect_err("expected an error").to_string(), "Resource server URL must use https");

    let mut relaxed = McpOAuthProvider::new(McpTokenStore::new(root.path(), "https", &url));
    relaxed.require_https = false;
    discover(&relaxed, &client).await.expect("test fixture");

    // A strict provider is also held to the rule when the info comes from the cache.
    assert_eq!(discover(&strict, &client).await.expect_err("expected an error").to_string(), "Resource server URL must use https");
    task.abort();
}
