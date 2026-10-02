use super::{cli_args::{Listen, WsAuth}, runtime::AppServerRuntime, unix_socket::start_unix_socket_listener, websocket::start_websocket_listener, websocket_auth::{WebSocketListenerAuth, resolve_websocket_listener_auth}};
use std::{future::Future, path::Path};

pub async fn run_app_server_mode(runtime: &AppServerRuntime, listen: Listen, auth: Option<WsAuth>, shutdown: impl Future<Output = ()>) -> Result<(), String> {
    let result=run_mode(runtime,listen,auth,shutdown).await;
    runtime.dispose().await;
    result
}
async fn shutdown_deadline(task:impl Future<Output=Result<(),String>>) -> Result<(),String> {
    tokio::time::timeout(std::time::Duration::from_secs(5),task).await
        .map_err(|_|"app-server shutdown exceeded 5000ms".to_owned())?
}
async fn run_mode(runtime: &AppServerRuntime, listen: Listen, auth: Option<WsAuth>, shutdown: impl Future<Output = ()>) -> Result<(), String> {
    let agent_dir = runtime.threads.agent_dir.clone();
    if matches!(listen,Listen::Stdio {..}) {
        tokio::select! {
            result = super::stdio::run_shared_stdio(runtime.core.clone(),tokio::io::stdin(),tokio::io::stdout()) => result.map_err(|error|error.message)?,
            _ = shutdown => {},
        }
        shutdown_deadline(async {runtime.threads.abort_active_turns().await;Ok(())}).await?;
        return Ok(());
    }
    let token_path = Path::new(&agent_dir).join("app-server/ws-token");
    let auth = match auth {Some(WsAuth::Off) => Some(WebSocketListenerAuth::Off),Some(WsAuth::TokenFile {path}) => Some(WebSocketListenerAuth::TokenFile(path.into())),None => None};
    let auth = resolve_websocket_listener_auth(auth,Some(&token_path)).await.map_err(|error|error.to_string())?;
    match listen {
        Listen::Unix {path,..} => {
            let path = path.map(Into::into).unwrap_or_else(||Path::new(&agent_dir).join("app-server/app-server.sock"));
            let listener = start_unix_socket_listener(path,true,auth,runtime.core.clone(),None).await.map_err(|error|error.message)?;
            shutdown.await;
            shutdown_deadline(async {runtime.threads.abort_active_turns().await;Ok(())}).await?;
            shutdown_deadline(async {listener.close().await.map_err(|error|error.message)}).await?;
        },
        Listen::Ws {host,port,..} => {
            let listener = start_websocket_listener(&host,port,auth,runtime.core.clone(),None).await.map_err(|error|error.message)?;
            shutdown.await;
            shutdown_deadline(async {runtime.threads.abort_active_turns().await;Ok(())}).await?;
            shutdown_deadline(async {listener.close().await.map_err(|error|error.message)}).await?;
        },
        Listen::Stdio {..} => unreachable!("stdio was handled before auth resolution"),
    }
    Ok(())
}
