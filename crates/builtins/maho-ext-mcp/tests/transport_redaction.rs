use std::{sync::{Arc, Mutex}, time::Duration};
use maho_ext_mcp::{log::{McpLogger, redact_url, redact_urls_in_text}, transport_sdk::{McpClient, McpTransportSpec}};

#[test]
fn sensitive_query_values_are_masked_and_the_rest_is_untouched() {
    assert_eq!(redact_url("https://a.test/mcp?api_key=s1&x=1&Token=s2&client_SECRET=s3"), "https://a.test/mcp?api_key=***REDACTED***&x=1&Token=***REDACTED***&client_SECRET=***REDACTED***");
    assert_eq!(redact_url("https://a.test/mcp?x=1&y=2"), "https://a.test/mcp?x=1&y=2");
    assert_eq!(redact_url("https://a.test/mcp"), "https://a.test/mcp");
    assert_eq!(redact_url("https://a.test/mcp?token=s#frag"), "https://a.test/mcp?token=***REDACTED***#frag");
}

#[test]
fn urls_inside_text_end_at_whitespace_or_delimiters() {
    let text = r#"error sending request for url (https://a.test/m?token=abc&q=1) and "http://b.test/?secret=zz" plain words key=notaurl"#;
    assert_eq!(
        redact_urls_in_text(text),
        r#"error sending request for url (https://a.test/m?token=***REDACTED***&q=1) and "http://b.test/?secret=***REDACTED***" plain words key=notaurl"#
    );
    assert_eq!(redact_urls_in_text("no urls here"), "no urls here");
}

#[tokio::test]
async fn http_send_failures_do_not_leak_url_secrets() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let root = tempfile::tempdir().unwrap();
    let logger = Arc::new(Mutex::new(McpLogger::new("redact", root.path(), None).unwrap()));
    let spec = McpTransportSpec::Http { url: url::Url::parse(&format!("http://127.0.0.1:{port}/mcp?api_key=SUPERSECRET&v=1")).unwrap(), headers: Default::default() };
    let client = McpClient::materialize("redact", &spec, logger).await.unwrap();
    let error = tokio::time::timeout(Duration::from_secs(10), client.request("ping", serde_json::json!({}), Duration::from_secs(5))).await.unwrap().unwrap_err();
    assert!(!error.message.contains("SUPERSECRET"), "{}", error.message);
}
