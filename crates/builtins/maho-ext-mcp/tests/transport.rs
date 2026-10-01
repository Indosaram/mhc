use std::collections::BTreeMap;
use maho_ext_mcp::{config_schema::*,transport::*,transport_sdk::*,errors::McpErrorKind};
#[test]
fn invalid_endpoints_fail_with_create_phase() {
    for config in [McpServerConfig {transport:Some(Transport::Stdio),command:Some(" ".into()),..Default::default()},McpServerConfig {transport:Some(Transport::Http),url:Some("nota url".into()),..Default::default()}] {
        let error=match create_mcp_transport_spec("bad",&config,None){Ok(_)=>panic!("accepted invalid endpoint"),Err(error)=>error};assert_eq!(error.kind,McpErrorKind::Connect);assert_eq!(error.phase.as_deref(),Some("create"));assert_eq!(error.server_name.as_deref(),Some("bad"));
    }
}
#[test]
fn stdio_config_env_overrides_caller_without_ambient_expansion() {
    let config=McpServerConfig {transport:Some(Transport::Stdio),command:Some("node".into()),env:Some(BTreeMap::from([("X".into(),"config".into())])),..Default::default()};
    let spec=create_mcp_transport_spec("srv",&config,Some(&BTreeMap::from([("X".into(),"caller".into())]))).unwrap();let McpTransportSpec::Stdio {env,..}=spec else{panic!("wrong kind")};assert_eq!(env["X"],"config");assert_eq!(env.len(),1);
}
#[test]
fn bearer_environment_is_resolved_per_creation() {
    let config=McpServerConfig {transport:Some(Transport::Http),url:Some("https://example.test".into()),bearer_token_env:Some("TOKEN".into()),..Default::default()};
    for token in ["first","second"] {let env=BTreeMap::from([("TOKEN".into(),token.into())]);let McpTransportSpec::Http {headers,..}=create_mcp_transport_spec("srv",&config,Some(&env)).unwrap() else{panic!("wrong kind")};assert_eq!(headers["authorization"],format!("Bearer {token}"));}
}
#[test]
fn auth_false_does_not_resolve_bearer_variable() {
    let config=McpServerConfig {transport:Some(Transport::Http),url:Some("https://example.test".into()),auth:Some(Auth::Disabled(false)),bearer_token_env:Some("MCP_FIXTURE_UNSET".into()),..Default::default()};
    let McpTransportSpec::Http {headers,..}=create_mcp_transport_spec("srv",&config,None).unwrap() else{panic!("wrong kind")};assert!(headers.is_empty());
}
#[tokio::test]
async fn native_stdio_connects_to_pinned_senpi_fixture() {
    use std::{sync::{Arc,Mutex},time::Duration};
    let root=tempfile::tempdir().unwrap();let logger=Arc::new(Mutex::new(maho_ext_mcp::log::McpLogger::new("fixture",root.path(),None).unwrap()));
    let config=McpServerConfig {transport:Some(Transport::Stdio),command:Some("/usr/bin/node".into()),args:Some(vec!["/home/indo/code/senpi/packages/coding-agent/test/mcp/fixtures/stdio-server.ts".into(),"--tools".into(),"2".into()]),connect_timeout_ms:Some(5000.0),..Default::default()};
    let connection=create_mcp_transport("fixture",&config,None,logger).unwrap();
    let client=connect_mcp_transport(&connection).await.unwrap();
    let tools=client.request("tools/list",serde_json::json!({}),Duration::from_secs(3)).await.unwrap();
    assert_eq!(tools["tools"].as_array().unwrap().iter().map(|tool|tool["name"].as_str().unwrap()).collect::<Vec<_>>(),vec!["tool_1","tool_2"]);
    let result=client.request("tools/call",serde_json::json!({"name":"tool_1","arguments":{"value":"native"}}),Duration::from_secs(3)).await.unwrap();assert!(result["content"].is_array());
    let catalog=maho_ext_mcp::catalog::collect_tool_catalog("fixture",client.clone(),Duration::from_secs(3)).await.unwrap();assert_eq!(catalog.len(),2);assert_eq!(catalog[0].tool,"tool_1");
    let cached=maho_ext_mcp::catalog_cache::collect_server_catalog_for_cache(&client,Duration::from_secs(3),"fixture-hash").await.unwrap();assert_eq!(cached.tools.len(),2);assert_eq!(cached.config_hash,"fixture-hash");
    let resource=maho_ext_mcp::resources::McpResourceServer {server:"fixture".into(),client:client.clone(),agent_dir:root.path().into(),artifacts:Arc::new(maho_ext_mcp::guard::output_guard::McpOutputArtifacts::default()),output_guard:None,request_timeout:Duration::from_secs(3),resources:cached.resources};
    let body=maho_ext_mcp::resources::read_mcp_resource_as_text(&resource,"fixture://resource/one").await.unwrap();assert_eq!(body,"resource body for fixture://resource/one");
    let expansion=maho_ext_mcp::resources::expand_mcp_resource_mentions("Use @mcp:fixture/fixture://resource/one please",&[resource]).await.unwrap();assert!(expansion.changed);assert!(expansion.text.contains(&body));assert!(expansion.notices.is_empty());
    shutdown_mcp_transport(&connection).await.unwrap();
}
#[tokio::test]
async fn native_http_connects_lists_and_calls_pinned_fixture() {
    use std::{sync::{Arc,Mutex},time::Duration,process::Stdio};
    use tokio::io::{AsyncBufReadExt,BufReader};
    let mut fixture=tokio::process::Command::new("/usr/bin/node").args(["/home/indo/code/senpi/packages/coding-agent/test/mcp/fixtures/http-server.ts","--tools","3"]).stdout(Stdio::piped()).stderr(Stdio::null()).kill_on_drop(true).spawn().unwrap();
    let output=fixture.stdout.take().unwrap();let mut lines=BufReader::new(output).lines();
    let ready=tokio::time::timeout(Duration::from_secs(5),lines.next_line()).await.unwrap().unwrap().unwrap();let ready:serde_json::Value=serde_json::from_str(&ready).unwrap();
    let root=tempfile::tempdir().unwrap();let logger=Arc::new(Mutex::new(maho_ext_mcp::log::McpLogger::new("http",root.path(),None).unwrap()));
    let config=McpServerConfig {transport:Some(Transport::Http),url:Some(ready["url"].as_str().unwrap().into()),auth:Some(Auth::Disabled(false)),connect_timeout_ms:Some(5000.0),..Default::default()};
    let connection=create_mcp_transport("http",&config,None,logger).unwrap();let client=connect_mcp_transport(&connection).await.unwrap();
    let tools=client.request("tools/list",serde_json::json!({}),Duration::from_secs(3)).await.unwrap();assert_eq!(tools["tools"].as_array().unwrap().len(),3);
    let result=client.request("tools/call",serde_json::json!({"name":"tool_1","arguments":{"value":"http native"}}),Duration::from_secs(3)).await.unwrap();assert!(result["content"].is_array());
    shutdown_mcp_transport(&connection).await.unwrap();fixture.kill().await.unwrap();fixture.wait().await.unwrap();
}
#[tokio::test]
async fn shutdown_reaps_fixture_process_tree() {
    use std::sync::{Arc,Mutex};
    let root=tempfile::tempdir().unwrap();let logger=Arc::new(Mutex::new(maho_ext_mcp::log::McpLogger::new("tree",root.path(),None).unwrap()));
    let config=McpServerConfig {transport:Some(Transport::Stdio),command:Some("/usr/bin/node".into()),args:Some(vec!["/home/indo/code/senpi/packages/coding-agent/test/mcp/fixtures/stdio-server.ts".into(),"--spawn-grandchild".into()]),connect_timeout_ms:Some(5000.0),..Default::default()};
    let connection=create_mcp_transport("tree",&config,None,logger).unwrap();connect_mcp_transport(&connection).await.unwrap();
    let pids=maho_ext_mcp::process_tree::collect_process_tree(connection.get_root_pid().unwrap()).await;assert!(pids.len()>1);
    shutdown_mcp_transport(&connection).await.unwrap();
    for pid in pids {assert!(!maho_ext_mcp::process_tree::is_process_alive(pid).await,"fixture pid {pid} still alive");}
}
