use std::{sync::{Arc,Mutex,atomic::{AtomicUsize,Ordering}},time::Duration};
use axum::{Router,routing::post,Json,response::IntoResponse,http::StatusCode};
use maho_ext_mcp::{config_schema::*,connection::{ServerConnection,ServerConnectionState},health::{McpHealthState,MCP_PING_STALE_MS},log::McpLogger};
use serde_json::{Value,json};

async fn health_fixture()->(String,Arc<AtomicUsize>,tokio::sync::oneshot::Sender<()>,tokio::task::JoinHandle<()>) {
    let pings=Arc::new(AtomicUsize::new(0));let observed=pings.clone();
    let app=Router::new().route("/mcp",post(move|Json(request):Json<Value>|{let observed=observed.clone();async move {
        if request.get("id").is_none(){return StatusCode::ACCEPTED.into_response();}
        let method=request["method"].as_str().unwrap_or_default();if method=="ping"{observed.fetch_add(1,Ordering::SeqCst);}
        let result=match method {"initialize"=>json!({"protocolVersion":"2025-11-25","capabilities":{},"serverInfo":{"name":"health","version":"1"}}),_=>json!({})};
        Json(json!({"jsonrpc":"2.0","id":request["id"],"result":result})).into_response()
    }}));
    let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("health listener");let address=listener.local_addr().expect("listener address");let (stop,stopped)=tokio::sync::oneshot::channel();
    let task=tokio::spawn(async move {axum::serve(listener,app).with_graceful_shutdown(async {let _=stopped.await;}).await.expect("health server");});
    (format!("http://{address}/mcp"),pings,stop,task)
}

fn connection(url:String,root:&std::path::Path)->Arc<ServerConnection>{
    ServerConnection::new("health",McpServerConfig {enabled:Some(true),transport:Some(Transport::Http),url:Some(url),auth:Some(Auth::Disabled(false)),connect_timeout_ms:Some(2000.0),..Default::default()},None,Arc::new(Mutex::new(McpLogger::new("health",root,None).expect("health logger"))))
}

#[tokio::test]
async fn successful_ping_is_cached_within_health_window(){
    let (url,pings,stop,server)=health_fixture().await;let root=tempfile::tempdir().unwrap();let connection=connection(url,root.path());let health=McpHealthState::default();
    health.ensure_connection(&connection).await.unwrap();assert_eq!(connection.state(),ServerConnectionState::Connected);assert_eq!(pings.load(Ordering::SeqCst),1);
    health.ensure_connection(&connection).await.unwrap();assert_eq!(pings.load(Ordering::SeqCst),1);
    connection.dispose().await.unwrap();stop.send(()).unwrap();tokio::time::timeout(Duration::from_secs(2),server).await.unwrap().unwrap();
}

#[tokio::test]
async fn stale_successful_ping_window_triggers_one_revalidation(){
    let (url,pings,stop,server)=health_fixture().await;let root=tempfile::tempdir().unwrap();let connection=connection(url,root.path());let health=McpHealthState::default();
    health.ensure_connection(&connection).await.unwrap();tokio::time::pause();tokio::time::advance(Duration::from_millis(MCP_PING_STALE_MS+1)).await;tokio::time::resume();
    health.ensure_connection(&connection).await.unwrap();assert_eq!(pings.load(Ordering::SeqCst),2);
    connection.dispose().await.unwrap();stop.send(()).unwrap();tokio::time::timeout(Duration::from_secs(2),server).await.unwrap().unwrap();
}

#[tokio::test]
async fn degraded_connection_is_reconnected_by_health_check(){
    let (url,_,stop,server)=health_fixture().await;let root=tempfile::tempdir().unwrap();let connection=connection(url,root.path());let health=McpHealthState::default();
    health.ensure_connection(&connection).await.unwrap();connection.mark_failure(ServerConnectionState::Degraded,None);
    health.ensure_connection(&connection).await.unwrap();assert_eq!(connection.state(),ServerConnectionState::Connected);assert_eq!(connection.generation(),1);
    connection.dispose().await.unwrap();stop.send(()).unwrap();tokio::time::timeout(Duration::from_secs(2),server).await.unwrap().unwrap();
}

#[test]
fn native_health_stdio_peer(){
    if std::env::var("MHC_HEALTH_STDIO_PEER").as_deref()!=Ok("1"){return;}
    use std::io::{BufRead,Write};
    let mut output=std::io::stdout().lock();
    for line in std::io::stdin().lock().lines(){
        let request:Value=serde_json::from_str(&line.unwrap()).unwrap();
        if request.get("id").is_none(){continue;}
        let result=if request["method"]=="initialize"{json!({"protocolVersion":"2025-11-25","capabilities":{},"serverInfo":{"name":"health-stdio","version":"1"}})}else{json!({})};
        writeln!(output,"{}",json!({"jsonrpc":"2.0","id":request["id"],"result":result})).unwrap();
        output.flush().unwrap();
    }
}

#[tokio::test]
async fn killed_native_process_is_observed_and_renewed(){
    let root=tempfile::tempdir().unwrap();
    let connection=ServerConnection::new("health-stdio",McpServerConfig{
        enabled:Some(true),transport:Some(Transport::Stdio),command:Some(std::env::current_exe().unwrap().to_string_lossy().into_owned()),
        args:Some(vec!["--exact".into(),"native_health_stdio_peer".into(),"--nocapture".into()]),
        env:Some(std::collections::BTreeMap::from([("MHC_HEALTH_STDIO_PEER".into(),"1".into())])),
        connect_timeout_ms:Some(2000.0),..Default::default()
    },None,Arc::new(Mutex::new(McpLogger::new("health-stdio",root.path(),None).unwrap())));
    let health=McpHealthState::default();
    health.ensure_connection(&connection).await.unwrap();
    let first=connection.get_root_pid().unwrap();
    let mut states=connection.on_state_change();
    assert!(std::process::Command::new("kill").args(["-KILL","--",&first.to_string()]).status().unwrap().success());
    tokio::time::timeout(Duration::from_secs(5),async{
        loop{if states.recv().await.unwrap().state==ServerConnectionState::Degraded{break;}}
    }).await.unwrap();
    health.ensure_connection(&connection).await.unwrap();
    assert_eq!(connection.state(),ServerConnectionState::Connected);
    assert_ne!(connection.get_root_pid().unwrap(),first);
    assert_eq!(connection.generation(),1);
    connection.dispose().await.unwrap();
}
