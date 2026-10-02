use std::{sync::{Arc,Mutex},time::Duration};
use maho_ext_api::{ToolCall,AbortSignal,ToolContent};
use maho_ext_mcp::{config_schema::*,transport::*,catalog::collect_tool_catalog,log::McpLogger,guard::output_guard::McpOutputArtifacts,expose::register::*};
use serde_json::json;
#[tokio::test]
async fn native_tool_registration_calls_the_real_fixture_and_rejects_errors_before_spill() {
    let root=tempfile::tempdir().unwrap();
    let config=McpServerConfig {transport:Some(Transport::Stdio),command:Some("/usr/bin/node".into()),args:Some(vec!["/home/indo/code/senpi/packages/coding-agent/test/mcp/fixtures/stdio-server.ts".into(),"--tools".into(),"1".into(),"--slow-tool-call".into(),"1".into()]),connect_timeout_ms:Some(5000.0),..Default::default()};
    let transport=create_mcp_transport("fixture",&config,None,Arc::new(Mutex::new(McpLogger::new("fixture",root.path(),None).unwrap()))).unwrap();
    let client=connect_mcp_transport(&transport).await.unwrap();
    let catalog=collect_tool_catalog("fixture",client,Duration::from_secs(3)).await.unwrap();
    let artifacts=Arc::new(McpOutputArtifacts::default());
    let tools=build_mcp_tool_definitions(&catalog,root.path().into(),artifacts.clone(),None);
    assert_eq!(tools[0].name,"mcp_fixture_tool_1");
    let updates=Arc::new(Mutex::new(Vec::new()));let capture=updates.clone();
    let result=(tools[0].execute)(ToolCall {id:"native",params:json!({"value":"native"}),signal:AbortSignal::default(),on_update:Some(Arc::new(move |update|{capture.lock().unwrap().push(update);Ok(())})),context:None}).await.unwrap();
    assert_eq!(updates.lock().unwrap().len(),1);assert_eq!(updates.lock().unwrap()[0].details.as_ref().unwrap()["progress"]["progress"],1);
    assert!(matches!(&result.content[0],ToolContent::Text {text,..} if text.contains("value=native")));
    let left_updates=Arc::new(Mutex::new(Vec::new()));let right_updates=Arc::new(Mutex::new(Vec::new()));let left=left_updates.clone();let right=right_updates.clone();
    let (left_result,right_result)=tokio::join!(
        (tools[0].execute)(ToolCall {id:"same",params:json!({"value":"left"}),signal:AbortSignal::default(),on_update:Some(Arc::new(move |update|{left.lock().unwrap().push(update);Ok(())})),context:None}),
        (tools[0].execute)(ToolCall {id:"same",params:json!({"value":"right"}),signal:AbortSignal::default(),on_update:Some(Arc::new(move |update|{right.lock().unwrap().push(update);Ok(())})),context:None}));
    left_result.unwrap();right_result.unwrap();{
        let left=left_updates.lock().unwrap();let right=right_updates.lock().unwrap();assert_eq!(left.len(),1);assert_eq!(right.len(),1);assert_ne!(left[0].details.as_ref().unwrap()["progress"]["progressToken"],right[0].details.as_ref().unwrap()["progress"]["progressToken"]);
    }
    let guard=OutputGuardSettings {max_bytes:Some(1.0),max_lines:Some(1.0),max_tokens:None};
    let failure=mapped_guarded_result(&catalog[0],&json!({"isError":true,"content":[{"type":"text","text":"error text that would otherwise spill"}]}),root.path(),&artifacts,Some(&guard));
    assert!(failure.is_err());assert!(!root.path().join("tmp/mcp-out").exists());
    shutdown_mcp_transport(&transport).await.unwrap();artifacts.cleanup().unwrap();
}
#[tokio::test]
async fn native_tool_runtime_reconnects_after_idle_generation_change() {
    let root=tempfile::tempdir().unwrap();
    let config=McpServerConfig {enabled:Some(true),transport:Some(Transport::Stdio),command:Some("/usr/bin/node".into()),args:Some(vec!["/home/indo/code/senpi/packages/coding-agent/test/mcp/fixtures/stdio-server.ts".into(),"--tools".into(),"1".into()]),connect_timeout_ms:Some(5000.0),..Default::default()};
    let connection=maho_ext_mcp::connection::ServerConnection::new("runtime",config.clone(),None,Arc::new(Mutex::new(McpLogger::new("runtime",root.path(),None).unwrap())));
    let client=connection.connect().await.unwrap();
    let mut catalog=collect_tool_catalog("runtime",client,Duration::from_secs(3)).await.unwrap();
    let lifecycle=maho_ext_mcp::idle::McpConnectionLifecycle::configure(connection.clone(),config);
    catalog[0].runtime=Some(Arc::new(maho_ext_mcp::catalog::McpCatalogRuntime {connection:connection.clone(),lifecycle:lifecycle.clone(),health:Default::default()}));
    let artifacts=Arc::new(McpOutputArtifacts::default());let tools=build_mcp_tool_definitions(&catalog,root.path().into(),artifacts.clone(),None);
    connection.bump_generation().await.unwrap();
    let result=(tools[0].execute)(ToolCall {id:"reconnect",params:json!({"value":"fresh-client"}),signal:AbortSignal::default(),on_update:None,context:None}).await.unwrap();
    assert!(matches!(&result.content[0],ToolContent::Text {text,..} if text.contains("value=fresh-client")));
    assert_eq!(connection.state(),maho_ext_mcp::connection::ServerConnectionState::Connected);assert_eq!(lifecycle.in_flight(),0);
    lifecycle.dispose();connection.dispose().await.unwrap();artifacts.cleanup().unwrap();
}
#[tokio::test]
async fn real_error_tool_execution_never_spills_output() {
    let root=tempfile::tempdir().unwrap();
    let config=McpServerConfig {transport:Some(Transport::Stdio),command:Some("/usr/bin/node".into()),args:Some(vec!["/home/indo/code/senpi/packages/coding-agent/test/mcp/fixtures/stdio-server.ts".into(),"--iserror-tool".into()]),connect_timeout_ms:Some(5000.0),..Default::default()};
    let transport=create_mcp_transport("errors",&config,None,Arc::new(Mutex::new(McpLogger::new("errors",root.path(),None).unwrap()))).unwrap();
    let client=connect_mcp_transport(&transport).await.unwrap();let catalog=collect_tool_catalog("errors",client,Duration::from_secs(3)).await.unwrap();
    let artifacts=Arc::new(McpOutputArtifacts::default());
    let tools=build_mcp_tool_definitions(&catalog,root.path().into(),artifacts.clone(),Some(OutputGuardSettings {max_bytes:Some(1.0),max_lines:Some(1.0),max_tokens:None}));
    let tool=tools.iter().find(|tool|tool.name=="mcp_errors_iserror_tool").unwrap();
    let result=(tool.execute)(ToolCall {id:"error",params:json!({}),signal:Default::default(),on_update:None,context:None}).await;
    shutdown_mcp_transport(&transport).await.unwrap();artifacts.cleanup().unwrap();
    assert!(result.is_err());assert!(!root.path().join("tmp/mcp-out").exists());
}
#[tokio::test]
async fn native_tool_runtime_suspends_when_reinitialized_session_expires_again() {
    use tokio::io::{AsyncBufReadExt,BufReader};
    let mut fixture=tokio::process::Command::new("/usr/bin/node").args(["/home/indo/code/senpi/packages/coding-agent/test/mcp/fixtures/http-server.ts","--tools","1","--always-expire-tool-calls"]).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::null()).kill_on_drop(true).spawn().unwrap();
    let mut lines=BufReader::new(fixture.stdout.take().unwrap()).lines();
    let ready=tokio::time::timeout(Duration::from_secs(5),lines.next_line()).await.unwrap().unwrap().unwrap();let ready:serde_json::Value=serde_json::from_str(&ready).unwrap();
    let root=tempfile::tempdir().unwrap();
    let config=McpServerConfig {enabled:Some(true),transport:Some(Transport::Http),url:Some(ready["url"].as_str().unwrap().into()),auth:Some(Auth::Disabled(false)),connect_timeout_ms:Some(5000.0),..Default::default()};
    let connection=maho_ext_mcp::connection::ServerConnection::new("expiry",config.clone(),None,Arc::new(Mutex::new(McpLogger::new("expiry",root.path(),None).unwrap())));
    let client=connection.connect().await.unwrap();let mut catalog=collect_tool_catalog("expiry",client,Duration::from_secs(3)).await.unwrap();
    let lifecycle=maho_ext_mcp::idle::McpConnectionLifecycle::configure(connection.clone(),config);
    catalog[0].runtime=Some(Arc::new(maho_ext_mcp::catalog::McpCatalogRuntime {connection:connection.clone(),lifecycle:lifecycle.clone(),health:Default::default()}));
    let artifacts=Arc::new(McpOutputArtifacts::default());let tools=build_mcp_tool_definitions(&catalog,root.path().into(),artifacts.clone(),None);
    let result=(tools[0].execute)(ToolCall {id:"expire",params:json!({}),signal:AbortSignal::default(),on_update:None,context:None}).await;
    assert!(result.is_err());assert_eq!(connection.generation(),1);assert_eq!(connection.state(),maho_ext_mcp::connection::ServerConnectionState::Suspended);assert_eq!(lifecycle.in_flight(),0);
    lifecycle.dispose();connection.dispose().await.unwrap();artifacts.cleanup().unwrap();fixture.kill().await.unwrap();fixture.wait().await.unwrap();
}
#[tokio::test]
async fn cached_native_tool_connects_only_on_execute_and_uses_entry_artifacts() {
    let root=tempfile::tempdir().unwrap();let artifact_root=tempfile::tempdir().unwrap();
    let config=McpServerConfig {enabled:Some(true),transport:Some(Transport::Stdio),command:Some("/usr/bin/node".into()),args:Some(vec!["/home/indo/code/senpi/packages/coding-agent/test/mcp/fixtures/stdio-server.ts".into(),"--tools".into(),"1".into()]),connect_timeout_ms:Some(5000.0),..Default::default()};
    let connection=maho_ext_mcp::connection::ServerConnection::new("cached",config.clone(),None,Arc::new(Mutex::new(McpLogger::new("cached",root.path(),None).unwrap())));let lifecycle=maho_ext_mcp::idle::McpConnectionLifecycle::configure(connection.clone(),config);
    let catalog=maho_ext_mcp::catalog_cache::McpCachedServerCatalog {config_hash:"hash".into(),fetched_at:0.0,tools:vec![json!({"name":"tool_1","inputSchema":{"type":"object"}})],resources:vec![],prompts:vec![],instructions:None};
    let runtime=Arc::new(maho_ext_mcp::catalog::McpCatalogRuntime {connection:connection.clone(),lifecycle:lifecycle.clone(),health:Default::default()});let connect=connection.clone();
    let mut entries=maho_ext_mcp::catalog::cached_mcp_catalog_entries("cached",&catalog,runtime,Duration::from_secs(3),Arc::new(move ||{let connection=connect.clone();Box::pin(async move {connection.connect().await?;Ok(())})}));
    let artifacts=Arc::new(McpOutputArtifacts::default());entries[0].agent_dir=Some(artifact_root.path().into());entries[0].artifacts=Some(artifacts.clone());entries[0].output_guard=Some(maho_ext_mcp::config_schema::OutputGuardSettings {max_bytes:Some(1.0),max_lines:None,max_tokens:None});
    let tools=build_mcp_tool_definitions(&entries,root.path().into(),Arc::new(McpOutputArtifacts::default()),None);assert_eq!(connection.state(),maho_ext_mcp::connection::ServerConnectionState::Idle);
    let result=(tools[0].execute)(ToolCall {id:"lazy",params:json!({"value":"lazy-call"}),signal:AbortSignal::default(),on_update:None,context:None}).await.unwrap();
    assert!(matches!(&result.content[0],ToolContent::Text {text,..} if text.contains("Full output saved to:")));assert!(artifact_root.path().join("tmp/mcp-out").exists());assert!(!root.path().join("tmp/mcp-out").exists());
    lifecycle.dispose();connection.dispose().await.unwrap();artifacts.cleanup().unwrap();
}
