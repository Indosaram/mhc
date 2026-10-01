use std::{sync::{Arc,Mutex},time::Duration};
use maho_ext_api::{ToolCall,AbortSignal,ToolContent};
use maho_ext_mcp::{config_schema::*,transport::*,catalog::collect_tool_catalog,log::McpLogger,guard::output_guard::McpOutputArtifacts,expose::register::*};
use serde_json::json;
#[tokio::test]
async fn native_tool_registration_calls_the_real_fixture_and_rejects_errors_before_spill() {
    let root=tempfile::tempdir().unwrap();
    let config=McpServerConfig {transport:Some(Transport::Stdio),command:Some("/usr/bin/node".into()),args:Some(vec!["/home/indo/code/senpi/packages/coding-agent/test/mcp/fixtures/stdio-server.ts".into(),"--tools".into(),"1".into()]),connect_timeout_ms:Some(5000.0),..Default::default()};
    let transport=create_mcp_transport("fixture",&config,None,Arc::new(Mutex::new(McpLogger::new("fixture",root.path(),None).unwrap()))).unwrap();
    let client=connect_mcp_transport(&transport).await.unwrap();
    let catalog=collect_tool_catalog("fixture",client,Duration::from_secs(3)).await.unwrap();
    let artifacts=Arc::new(McpOutputArtifacts::default());
    let tools=build_mcp_tool_definitions(&catalog,root.path().into(),artifacts.clone(),None);
    assert_eq!(tools[0].name,"mcp_fixture_tool_1");
    let result=(tools[0].execute)(ToolCall {id:"native",params:json!({"value":"native"}),signal:AbortSignal::default(),on_update:None,context:None}).await.unwrap();
    assert!(matches!(&result.content[0],ToolContent::Text {text,..} if text.contains("value=native")));
    let guard=OutputGuardSettings {max_bytes:Some(1.0),max_lines:Some(1.0),max_tokens:None};
    let failure=mapped_guarded_result(&catalog[0],&json!({"isError":true,"content":[{"type":"text","text":"error text that would otherwise spill"}]}),root.path(),&artifacts,Some(&guard));
    assert!(failure.is_err());assert!(!root.path().join("tmp/mcp-out").exists());
    shutdown_mcp_transport(&transport).await.unwrap();artifacts.cleanup().unwrap();
}
