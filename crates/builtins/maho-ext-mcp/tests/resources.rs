use maho_ext_mcp::resources::*;
use serde_json::json;
#[test]
fn binary_resource_preserves_placeholder_size() {
    assert_eq!(flatten_resource_contents(&[json!({"blob":"QUFBQQ==","mimeType":"image/png","uri":"fake://bin"})]),"[binary resource fake://bin (image/png), ~6 bytes]");
}
#[tokio::test]
async fn unknown_mentions_remain_unchanged_with_notice() {
    let result=expand_mcp_resource_mentions("See @mcp:nope/some://uri now",&[]).await.unwrap();assert!(!result.changed);assert_eq!(result.text,"See @mcp:nope/some://uri now");assert_eq!(result.notices.len(),1);
}
#[tokio::test]
async fn malformed_mentions_pass_through() {
    let result=expand_mcp_resource_mentions("See @mcp:missing-slash now",&[]).await.unwrap();assert!(!result.changed);assert!(result.notices.is_empty());
}
#[tokio::test]
async fn utility_tools_list_and_read_the_real_server() {
    use std::{sync::{Arc,Mutex},time::Duration};
    use maho_ext_api::{ToolCall,AbortSignal,ToolContent};
    use maho_ext_mcp::{config_schema::*,transport::*,catalog::collect_client_pages,log::McpLogger,guard::output_guard::McpOutputArtifacts};
    // Given a connected fixture exposing a resource.
    let root=tempfile::tempdir().unwrap();
    let config=McpServerConfig {transport:Some(Transport::Stdio),command:Some("/usr/bin/node".into()),args:Some(vec!["/home/indo/code/senpi/packages/coding-agent/test/mcp/fixtures/stdio-server.ts".into(),"--tools".into(),"1".into()]),connect_timeout_ms:Some(5000.0),..Default::default()};
    let transport=create_mcp_transport("fx",&config,None,Arc::new(Mutex::new(McpLogger::new("fx",root.path(),None).unwrap()))).unwrap();
    let client=connect_mcp_transport(&transport).await.unwrap();
    let resources=collect_client_pages(&client,"resources/list","resources",Duration::from_secs(3)).await.unwrap().items;
    let artifacts=Arc::new(McpOutputArtifacts::default());
    let server=McpResourceServer {server:"fx".into(),client,agent_dir:root.path().into(),artifacts:artifacts.clone(),output_guard:None,request_timeout:Duration::from_secs(3),resources};
    let tools=create_mcp_resource_tools(Arc::new(move ||vec![server.clone()]));
    let call=|params|ToolCall {id:"resource",params,signal:AbortSignal::default(),on_update:None,context:None};
    // When the native utility definitions list and read the resource.
    let list=(tools[0].execute)(call(json!({"server":"fx"}))).await.unwrap();
    let read=(tools[1].execute)(call(json!({"server":"fx","uri":"fixture://resource/one"}))).await.unwrap();
    // Then the catalog URI and actual server body are returned.
    assert!(matches!(&list.content[0],ToolContent::Text {text,..} if text.contains("@mcp:fx/fixture://resource/one")));
    assert_eq!(list.details.unwrap()["tool"],"mcp_list_resources");
    assert!(matches!(&read.content[0],ToolContent::Text {text,..} if text=="resource body for fixture://resource/one"));
    shutdown_mcp_transport(&transport).await.unwrap();artifacts.cleanup().unwrap();
}
#[tokio::test]
async fn utility_read_rejects_unknown_servers() {
    use maho_ext_api::{ToolCall,AbortSignal};
    // Given an empty resource inventory.
    let tools=create_mcp_resource_tools(std::sync::Arc::new(Vec::new));
    // When reading a resource from an unknown server.
    let result=(tools[1].execute)(ToolCall {id:"missing",params:json!({"server":"missing","uri":"fixture://one"}),signal:AbortSignal::default(),on_update:None,context:None}).await;
    // Then execution fails instead of returning a fabricated resource.
    assert_eq!(result.unwrap_err().to_string(),"Unknown MCP server 'missing' for resource fixture://one.");
}
