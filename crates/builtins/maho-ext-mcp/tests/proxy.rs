use std::{sync::{Arc, Mutex}, time::Duration};
use maho_ext_api::{AbortSignal, ToolCall, ToolContent};
use maho_ext_mcp::{catalog::collect_tool_catalog, config_schema::*, expose::{proxy::*, register::build_mcp_tool_definitions}, guard::output_guard::McpOutputArtifacts, log::McpLogger, transport::*};
use serde_json::json;

#[tokio::test]
async fn proxy_describe_and_call_use_real_catalog_and_guide_invalid_arguments() {
    let root = tempfile::tempdir().unwrap();
    let config = McpServerConfig {transport: Some(Transport::Stdio), command: Some("/usr/bin/node".into()), args: Some(vec!["/home/indo/code/senpi/packages/coding-agent/test/mcp/fixtures/stdio-server.ts".into(), "--tools".into(), "1".into()]), ..Default::default()};
    let transport = create_mcp_transport("fx", &config, None, Arc::new(Mutex::new(McpLogger::new("fx", root.path(), None).unwrap()))).unwrap();
    let client = connect_mcp_transport(&transport).await.unwrap();
    let catalog = collect_tool_catalog("fx", client, Duration::from_secs(3)).await.unwrap();
    let artifacts = Arc::new(McpOutputArtifacts::default());
    let full = build_mcp_tool_definitions(&catalog, root.path().into(), artifacts.clone(), None);
    let description = describe_mcp_proxy_entry(&catalog[0]).unwrap();
    assert!(matches!(&description.content[0], ToolContent::Text {text, ..} if text.contains("Input schema") && text.contains("value")));
    let call = || ToolCall {id: "proxy", params: json!({}), signal: AbortSignal::default(), on_update: None, context: None};
    let result = call_mcp_proxy_entry(&catalog[0], &full[0], Some(r#"{"value":"via-proxy"}"#), call()).await.unwrap();
    assert!(matches!(&result.content[0], ToolContent::Text {text, ..} if text == "fixture tool_1 value=via-proxy mode=alpha"));
    for args in ["{not json", "[]", "null", "1", "\"string\""] {
        let result = call_mcp_proxy_entry(&catalog[0], &full[0], Some(args), call()).await.unwrap();
        assert_eq!(result.details.as_ref().unwrap()["tool"], "proxy");
        assert!(matches!(&result.content[0], ToolContent::Text {text, ..} if text.contains("JSON object STRING")));
    }
    let result = call_mcp_proxy_entry(&catalog[0], &full[0], None, call()).await.unwrap();
    assert!(matches!(&result.content[0], ToolContent::Text {text, ..} if text.contains("fixture tool_1")));
    let signal = AbortSignal::default(); signal.abort();
    let result = call_mcp_proxy_entry(&catalog[0], &full[0], None, ToolCall {signal, ..call()}).await;
    assert!(result.is_err());
    shutdown_mcp_transport(&transport).await.unwrap(); artifacts.cleanup().unwrap();
}

#[test]
fn auto_exposure_never_selects_proxy() {
    #[derive(Clone)] struct Entry;
    impl maho_ext_mcp::expose::policy::CatalogIdentity for Entry {
        fn server(&self) -> &str {"big"} fn tool(&self) -> &str {"tool"}
    }
    let policy = maho_ext_mcp::expose::policy::compute_mcp_exposure_policy(&vec![Entry;40], &McpServerConfig {exposure: Some(Exposure::Auto), ..Default::default()}, &McpSettings::default());
    assert_ne!(policy.mode, Exposure::Proxy);
}
