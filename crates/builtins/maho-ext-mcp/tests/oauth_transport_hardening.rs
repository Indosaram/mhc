//! SC-U2 transport hardening regressions: the pinned `parseHttpsUrl` gate
//! (`mcp-oauth/discovery.ts`) and the pinned `redactUrl` URL-query redaction
//! (`skill-mcp-manager/http-client.ts`).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;

use maho_ext_mcp::auth::oauth::{discover, parse_https_url, parse_metadata_fields};
use maho_ext_mcp::auth::oauth_provider::McpOAuthProvider;
use maho_ext_mcp::auth::token_store::McpTokenStore;
use maho_ext_mcp::log::{redact_url, redact_urls_in_text, McpLogger};
use maho_ext_mcp::transport_sdk::{McpClient, McpTransportSpec};

#[test]
fn production_default_requires_https() {
    let root = tempfile::tempdir().unwrap();
    assert!(McpOAuthProvider::new(McpTokenStore::new(root.path(), "default", "https://example.test")).require_https);
}

#[test]
fn https_is_required_for_every_pinned_oauth_url_label() {
    for label in ["Resource server URL", "Authorization server URL", "authorization_endpoint", "token_endpoint", "registration_endpoint"] {
        assert_eq!(parse_https_url("http://example.test/x", label, true).unwrap_err().to_string(), format!("{label} must use https"));
        assert!(parse_https_url("https://example.test/x", label, true).is_ok());
        assert!(parse_https_url("http://127.0.0.1:1/x", label, false).is_ok());
    }
}

#[test]
fn metadata_endpoints_are_validated_and_normalized() {
    let http = json!({"authorization_endpoint": "http://example.test/a", "token_endpoint": "https://example.test/t"});
    assert_eq!(parse_metadata_fields(&http, true).unwrap_err().to_string(), "authorization_endpoint must use https");
    let registration = json!({"authorization_endpoint": "https://example.test/a", "token_endpoint": "https://example.test/t", "registration_endpoint": "http://example.test/r"});
    assert_eq!(parse_metadata_fields(&registration, true).unwrap_err().to_string(), "registration_endpoint must use https");
    let normalized = parse_metadata_fields(&json!({"authorization_endpoint": "https://example.test/a", "token_endpoint": "https://example.test/t"}), true).unwrap();
    assert_eq!(normalized["authorization_endpoint"], json!("https://example.test/a"));
    assert_eq!(normalized["token_endpoint"], json!("https://example.test/t"));
    let fixture = parse_metadata_fields(&json!({"authorization_endpoint": "http://127.0.0.1:1/a", "token_endpoint": "http://127.0.0.1:1/t"}), false).unwrap();
    assert_eq!(fixture["token_endpoint"], json!("http://127.0.0.1:1/t"));
}

#[tokio::test]
async fn discovery_rejects_an_http_resource_url_in_production() {
    let root = tempfile::tempdir().unwrap();
    let provider = McpOAuthProvider::new(McpTokenStore::new(root.path(), "https-required", "http://127.0.0.1:1/mcp"));
    assert_eq!(discover(&provider, &reqwest::Client::new()).await.unwrap_err().to_string(), "Resource server URL must use https");
}

#[test]
fn redact_url_masks_sensitive_query_params() {
    assert_eq!(redact_url("https://mcp.example.test/sse?access_token=SECRET&mode=1&apiKey=K"), "https://mcp.example.test/sse?access_token=***REDACTED***&mode=1&apiKey=***REDACTED***");
    assert_eq!(redact_url("https://mcp.example.test/sse"), "https://mcp.example.test/sse");
    assert_eq!(redact_url("not a url"), "not a url");
}

#[test]
fn redact_urls_in_text_masks_every_embedded_url() {
    let redacted = redact_urls_in_text("error sending request for url (http://127.0.0.1:9/mcp?token=SUPERSECRET&mode=1) and https://a.test/x?secret=S");
    assert!(!redacted.contains("SUPERSECRET"));
    assert!(!redacted.contains("secret=S"));
    assert!(redacted.contains("***REDACTED***"));
}

#[tokio::test]
async fn http_transport_errors_do_not_leak_url_query_secrets() {
    let root = tempfile::tempdir().unwrap();
    let logger = Arc::new(Mutex::new(McpLogger::new("redact", root.path(), None).unwrap()));
    let spec = McpTransportSpec::Http { url: "http://127.0.0.1:1/mcp?token=SUPERSECRET&mode=1".parse().unwrap(), headers: Default::default() };
    let client = McpClient::materialize("redact", &spec, logger).await.unwrap();
    let error = client.request("tools/list", json!({}), Duration::from_secs(5)).await.unwrap_err();
    assert!(!error.message.contains("SUPERSECRET"), "the transport error must not leak the query secret: {}", error.message);
}
