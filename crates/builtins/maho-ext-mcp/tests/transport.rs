use std::collections::BTreeMap;
use maho_ext_mcp::{config_schema::*,transport::*,transport_sdk::*,errors::McpErrorKind};
#[test]
fn invalid_endpoints_fail_with_create_phase() {
    for config in [McpServerConfig {transport:Some(Transport::Stdio),command:Some(" ".into()),..Default::default()},McpServerConfig {transport:Some(Transport::Http),url:Some("nota url".into()),..Default::default()}] {
        let error=match create_mcp_transport_spec("bad",&config,None){Ok(_)=>panic!("accepted invalid endpoint"),Err(error)=>error};assert_eq!(error.kind,McpErrorKind::Connect);assert_eq!(error.phase.as_deref(),Some("create"));assert_eq!(error.server_name.as_deref(),Some("bad"));
    }
}
#[tokio::test]
async fn cancellation_notifies_the_server_with_the_inflight_request_id() {
    use std::{sync::{Arc,Mutex},time::Duration};
    use serde_json::json;
    let root=tempfile::tempdir().unwrap();let logger=Arc::new(Mutex::new(maho_ext_mcp::log::McpLogger::new("cancel",root.path(),None).unwrap()));
    let config=McpServerConfig {transport:Some(Transport::Stdio),command:Some("/usr/bin/node".into()),args:Some(vec![format!("{}/tests/fixtures/cancellation.mjs",env!("CARGO_MANIFEST_DIR"))]),connect_timeout_ms:Some(5000.0),..Default::default()};
    let connection=create_mcp_transport("cancel",&config,None,logger).unwrap();let client=connect_mcp_transport(&connection).await.unwrap();let mut events=client.notifications.subscribe();
    let signal=maho_ext_api::AbortSignal::default();
    let request=client.request_with_signal("tools/call",json!({"name":"pending","_meta":{"progressToken":"cancel"}}),Duration::from_secs(5),&signal);
    let cancellation=async {
        let progress=events.recv().await.unwrap();assert_eq!(progress["method"],"notifications/progress");signal.abort();
        let cancelled=events.recv().await.unwrap();assert_eq!(cancelled["method"],"fixture/cancelled");assert_eq!(cancelled["params"]["matched"],true);assert!(cancelled["params"]["requestId"].is_number());
    };
    let (result,())=tokio::time::timeout(Duration::from_secs(5),async {tokio::join!(request,cancellation)}).await.unwrap();assert!(result.is_err());
    shutdown_mcp_transport(&connection).await.unwrap();
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
async fn stdio_oauth_materialization_passes_current_token_to_child() {
    use std::sync::{Arc,Mutex};
    let root=tempfile::tempdir().unwrap();
    let store=maho_ext_mcp::auth::token_store::McpTokenStore::new(root.path(),"oauth-env","https://fixture.test");
    store.write(maho_ext_mcp::auth::token_store::McpStoredAuth {access_token:Some("fixture-current".into()),..Default::default()}).unwrap();
    let provider=Arc::new(maho_ext_mcp::auth::oauth_provider::McpOAuthProvider::new(store));
    let config=McpServerConfig {transport:Some(Transport::Stdio),command:Some("/usr/bin/node".into()),args:Some(vec![format!("{}/tests/fixtures/oauth-env.mjs",env!("CARGO_MANIFEST_DIR"))]),..Default::default()};
    let mut transport=create_mcp_transport("oauth-env",&config,None,Arc::new(Mutex::new(maho_ext_mcp::log::McpLogger::new("oauth-env",root.path(),None).unwrap()))).unwrap();
    transport.auth=Some(Arc::new(maho_ext_mcp::auth::oauth_refresh::McpRefreshManager::new(provider,reqwest::Client::new())));
    let client=connect_mcp_transport(&transport).await.unwrap();
    assert_eq!(client.server_info.read().await["version"],"current");
    shutdown_mcp_transport(&transport).await.unwrap();
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
    let mut config=config;config.enabled=Some(true);
    let catalog_connection=maho_ext_mcp::connection::ServerConnection::new("fixture-cache",config,None,Arc::new(Mutex::new(maho_ext_mcp::log::McpLogger::new("fixture-cache",root.path(),None).unwrap())));catalog_connection.connect().await.unwrap();
    let cached=maho_ext_mcp::catalog_cache::collect_server_catalog_for_cache(&catalog_connection,Duration::from_secs(3),"fixture-hash").await.unwrap();assert_eq!(cached.tools.len(),2);assert_eq!(cached.config_hash,"fixture-hash");catalog_connection.dispose().await.unwrap();
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
#[tokio::test]
async fn http_get_stream_delivers_unsolicited_notifications() {
    use std::{sync::{Arc,Mutex},time::Duration};
    use axum::{Router,routing::get,response::{sse::{Sse,Event},IntoResponse},Json};
    use serde_json::{Value,json};
    let (opened,ready)=tokio::sync::oneshot::channel();let opened=Arc::new(Mutex::new(Some(opened)));
    let (sender,receiver)=tokio::sync::mpsc::unbounded_channel::<Value>();let receiver=Arc::new(tokio::sync::Mutex::new(Some(receiver)));
    let get_route=get(move ||{let opened=opened.clone();let receiver=receiver.clone();async move {
        let receiver=receiver.lock().await.take().unwrap();opened.lock().unwrap().take().unwrap().send(()).unwrap();
        Sse::new(futures::stream::unfold(receiver,|mut receiver|async move {receiver.recv().await.map(|value|(Ok::<_,std::convert::Infallible>(Event::default().id("one").data(value.to_string())),receiver))}))
    }}).post(|Json(value):Json<Value>|async move {
        if value.get("id").is_none(){return axum::http::StatusCode::ACCEPTED.into_response();}
        let result=if value["method"]=="initialize" {json!({"protocolVersion":"2025-11-25","capabilities":{},"serverInfo":{"name":"fixture","version":"1"}})}else{json!({})};
        Json(json!({"jsonrpc":"2.0","id":value.get("id").cloned().unwrap_or(Value::Null),"result":result})).into_response()
    });
    let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();let address=listener.local_addr().unwrap();
    let (stop,stopped)=tokio::sync::oneshot::channel();
    let server=tokio::spawn(async move {axum::serve(listener,Router::new().route("/mcp",get_route)).with_graceful_shutdown(async {let _=stopped.await;}).await.unwrap();});
    let root=tempfile::tempdir().unwrap();
    let client=McpClient::materialize("stream",&McpTransportSpec::Http {url:format!("http://{address}/mcp").parse().unwrap(),headers:Default::default()},Arc::new(Mutex::new(maho_ext_mcp::log::McpLogger::new("stream",root.path(),None).unwrap()))).await.unwrap();
    let mut notifications=client.notifications.subscribe();
    client.initialize(Duration::from_secs(3)).await.unwrap();
    tokio::time::timeout(Duration::from_secs(3),ready).await.unwrap().unwrap();
    sender.send(json!({"jsonrpc":"2.0","method":"notifications/tools/list_changed"})).unwrap();
    let event=tokio::time::timeout(Duration::from_secs(3),notifications.recv()).await.unwrap().unwrap();
    assert_eq!(event["method"],"notifications/tools/list_changed");
    client.close().await.unwrap();drop(client);drop(sender);stop.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(3),server).await.unwrap().unwrap();
}
#[tokio::test]
async fn concurrent_shutdowns_coalesce_and_wait_for_the_child() {
    use std::sync::{Arc,Mutex};
    let root=tempfile::tempdir().unwrap();
    let config=McpServerConfig {transport:Some(Transport::Stdio),command:Some("/usr/bin/node".into()),args:Some(vec!["/home/indo/code/senpi/packages/coding-agent/test/mcp/fixtures/stdio-server.ts".into()]),connect_timeout_ms:Some(5000.0),..Default::default()};
    let connection=create_mcp_transport("shutdown",&config,None,Arc::new(Mutex::new(maho_ext_mcp::log::McpLogger::new("shutdown",root.path(),None).unwrap()))).unwrap();
    connect_mcp_transport(&connection).await.unwrap();let pid=connection.get_root_pid().unwrap();
    let (first,second)=tokio::join!(shutdown_mcp_transport(&connection),shutdown_mcp_transport(&connection));first.unwrap();second.unwrap();
    assert!(!maho_ext_mcp::process_tree::is_process_alive(pid).await);
}
#[tokio::test]
async fn http_post_sse_accepts_bom_and_carriage_return_delimiters() {
    use axum::{Router,routing::post,Json,response::IntoResponse};
    use serde_json::{Value,json};use std::{sync::{Arc,Mutex},time::Duration};
    let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();let address=listener.local_addr().unwrap();
    let (stop,stopped)=tokio::sync::oneshot::channel();
    let server=tokio::spawn(async move {axum::serve(listener,Router::new().route("/mcp",post(|Json(value):Json<Value>|async move {
        if value.get("id").is_none(){return axum::http::StatusCode::ACCEPTED.into_response();}
        let result=if value["method"]=="initialize" {json!({"protocolVersion":"2025-11-25","capabilities":{},"serverInfo":{"name":"cr","version":"1"}})}else{json!({"tools":[]})};
        ([("content-type","text/event-stream")],format!("\u{feff}data: {}\r\r",json!({"jsonrpc":"2.0","id":value["id"],"result":result}))).into_response()
    }))).with_graceful_shutdown(async {let _=stopped.await;}).await.unwrap();});
    let root=tempfile::tempdir().unwrap();let client=McpClient::materialize("cr",&McpTransportSpec::Http {url:format!("http://{address}/mcp").parse().unwrap(),headers:Default::default()},Arc::new(Mutex::new(maho_ext_mcp::log::McpLogger::new("cr",root.path(),None).unwrap()))).await.unwrap();
    client.initialize(Duration::from_secs(2)).await.unwrap();assert_eq!(client.request("tools/list",json!({}),Duration::from_secs(2)).await.unwrap()["tools"],json!([]));
    client.close().await.unwrap();drop(client);stop.send(()).unwrap();tokio::time::timeout(Duration::from_secs(2),server).await.unwrap().unwrap();
}
