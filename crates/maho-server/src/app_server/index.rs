use super::{cli_args::{Listen, WsAuth}, runtime::AppServerRuntime, websocket::start_websocket_listener, websocket_auth::{ResolvedWebSocketListenerAuth, WebSocketListenerAuth, resolve_websocket_listener_auth_with_receipt}};
#[cfg(unix)]
use super::unix_socket::start_unix_socket_listener;
use std::{future::Future, path::{Path, PathBuf}};

pub enum BoundListen {
    Stdio,
    Ws { host: String, port: u16, token_file: Option<PathBuf> },
    Unix { socket_path: PathBuf, token_file: Option<PathBuf> },
}
/// senpi `runAppServerMode`'s listening banner, written to stderr after the transport is bound.
pub fn listening_banner(app_name: &str, bound: &BoundListen) -> String {
    let (mut banner, token_file) = match bound {
        BoundListen::Stdio => (format!("{app_name} app-server listening on stdio://\n"), None),
        BoundListen::Ws { host, port, token_file } => (format!("{app_name} app-server listening on ws://{host}:{port}\nreadyz http://127.0.0.1:{port}/readyz\n"), token_file.as_ref()),
        BoundListen::Unix { socket_path, token_file } => (format!("{app_name} app-server listening on unix://{}\n", socket_path.display()), token_file.as_ref()),
    };
    if let Some(token_file) = token_file { banner.push_str(&format!("token {}\n", token_file.display())); }
    banner
}
fn bearer_token_file(auth: &ResolvedWebSocketListenerAuth) -> Option<PathBuf> {
    match auth { ResolvedWebSocketListenerAuth::Bearer { path, .. } => path.clone(), ResolvedWebSocketListenerAuth::Off => None }
}

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
    let app_name = maho_core::config::app_name();
    if matches!(listen,Listen::Stdio {..}) {
        eprint!("{}", listening_banner(&app_name, &BoundListen::Stdio));
        super::stdio::run_shared_stdio_until(runtime.core.clone(),tokio::io::stdin(),tokio::io::stdout(),shutdown,None).await.map_err(|error|error.message)?;
        shutdown_deadline(async {runtime.threads.abort_active_turns().await;Ok(())}).await?;
        return Ok(());
    }
    // Pinned unix-socket.ts: the owner-only chmod applies only to the default socket directory
    // when no bearer auth is configured (the directory is then the only access guard).
    let auth_off_or_default = matches!(auth, None | Some(WsAuth::Off));
    let auth = match auth {Some(WsAuth::Off) => Some(WebSocketListenerAuth::Off),Some(WsAuth::TokenFile {path}) => Some(WebSocketListenerAuth::TokenFile(path.into())),None => None};
    match listen {
        Listen::Unix {path,..} => {
            #[cfg(unix)]
            {
            let default_path = Path::new(&agent_dir).join("app-server/app-server.sock");
            let path: PathBuf = path.map(Into::into).unwrap_or_else(||default_path.clone());
            let enforce_owner_only_directory = path == default_path && auth_off_or_default;
            // Pinned unix-socket.ts passes no `defaultTokenPath`: the default unix listener is unauthenticated.
            let auth = resolve_websocket_listener_auth_with_receipt(auth,None,"app-server unix socket websocket token",None).await.map_err(|error|error.to_string())?;
            let token_file = bearer_token_file(&auth);
            let listener = start_unix_socket_listener(path,enforce_owner_only_directory,auth,runtime.core.clone(),None).await.map_err(|error|error.message)?;
            eprint!("{}", listening_banner(&app_name, &BoundListen::Unix { socket_path: listener.socket_path.clone(), token_file }));
            shutdown.await;
            shutdown_deadline(async {runtime.threads.abort_active_turns().await;Ok(())}).await?;
            shutdown_deadline(async {listener.close().await.map_err(|error|error.message)}).await?;
            }
            #[cfg(not(unix))]
            { let _ = (path, auth, auth_off_or_default, shutdown); return Err("unix-socket transport is not supported on this platform yet".into()); }
        },
        Listen::Ws {host,port,..} => {
            let token_path = Path::new(&agent_dir).join("app-server/ws-token");
            let auth = resolve_websocket_listener_auth_with_receipt(auth,Some(&token_path),"app-server websocket token",None).await.map_err(|error|error.to_string())?;
            let token_file = bearer_token_file(&auth);
            let listener = start_websocket_listener(&host,port,auth,runtime.core.clone(),None).await.map_err(|error|error.message)?;
            eprint!("{}", listening_banner(&app_name, &BoundListen::Ws { host, port: listener.address.port(), token_file }));
            shutdown.await;
            shutdown_deadline(async {runtime.threads.abort_active_turns().await;Ok(())}).await?;
            shutdown_deadline(async {listener.close().await.map_err(|error|error.message)}).await?;
        },
        Listen::Stdio {..} => unreachable!("stdio was handled before auth resolution"),
    }
    Ok(())
}
