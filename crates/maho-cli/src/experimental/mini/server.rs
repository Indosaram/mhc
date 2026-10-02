use std::sync::Arc;
#[cfg(unix)]
use std::{collections::HashMap, sync::{Mutex, Weak}, path::{Path, PathBuf}};
#[cfg(unix)]
use super::shared::{rpc::{RpcPeer, Handler, HandlerFuture, CallOptions, PeerOptions, create_peer}, protocol::SESSIONS};
use maho_agent::harness::{context::BACKGROUND_CONTEXT, env::nodejs::NodeExecutionEnv, session::jsonl::{JsonlSessionRepo, JsonlSessionRepoOptions}, types::FileSystem};
use super::shared::protocol::SessionSummary;
pub async fn list_sessions(sessions_root: &str, cwd: &str) -> Result<Vec<SessionSummary>, String> {
    let env = Arc::new(NodeExecutionEnv::new(cwd));
    let repo = JsonlSessionRepo::new(JsonlSessionRepoOptions { file_system: env.clone(), sessions_root: sessions_root.to_owned(), now: None });
    let result = repo.list(None, &BACKGROUND_CONTEXT).await.map(|sessions| sessions.into_iter().map(|metadata| SessionSummary { id: metadata.id, path: metadata.path, cwd: metadata.cwd, created_at: metadata.created_at as f64 }).collect()).map_err(|error| error.to_string());
    repo.close(&BACKGROUND_CONTEXT).await;
    env.cleanup(&BACKGROUND_CONTEXT).await;
    result
}
#[cfg(unix)]
pub struct SpawnedWorker { pub peer: Arc<RpcPeer>, pub stop: Box<dyn Fn() + Send + Sync> }
#[cfg(unix)]
pub type SpawnWorker = Arc<dyn Fn(Option<String>, String) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<SpawnedWorker, String>> + Send>> + Send + Sync>;
#[cfg(unix)]
struct Route { id: String, worker: SpawnedWorker, subscribers: Mutex<HashMap<String, Weak<RpcPeer>>> }
#[cfg(unix)]
struct ServerState {
    routes: Mutex<HashMap<String, Arc<Route>>>,
    spawning: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    changed: tokio::sync::mpsc::UnboundedSender<()>,
    sessions_root: String, cwd: String, spawn: SpawnWorker,
}
#[cfg(unix)]
fn sessions_list(state: &Arc<ServerState>) -> Handler {
    let state = Arc::downgrade(state);
    Arc::new(move |_, _| {
        let state = state.clone();
        Box::pin(async move {
            let state = state.upgrade().ok_or("Server closed")?;
            serde_json::to_value(list_sessions(&state.sessions_root, &state.cwd).await?).map_err(|error| error.to_string())
        })
    })
}
#[cfg(unix)]
async fn spawn_route(state: &Arc<ServerState>, session_id: Option<String>, cwd: String) -> Result<Arc<Route>, String> {
    let worker = (state.spawn)(session_id, cwd).await?;
    let unsupported: Handler = Arc::new(|_, _| Box::pin(async { Err("Only presentations attach to sessions".to_owned()) }));
    worker.peer.provide(SESSIONS, HashMap::from([("list".to_owned(), sessions_list(state)), ("attach".to_owned(), unsupported)]));
    let described = worker.peer.call_with(CallOptions { timeout_ms: Some(30_000), ..Default::default() }, "worker.describe", vec![]).await?;
    let id = described["sessionId"].as_str().ok_or("Worker description requires sessionId")?.to_owned();
    let route = Arc::new(Route { id: id.clone(), worker, subscribers: Mutex::new(HashMap::new()) });
    let weak_route = Arc::downgrade(&route);
    route.worker.peer.on_event(move |service, payload, to| {
        if let Some(route) = weak_route.upgrade() {
            let subscribers = route.subscribers.lock().expect("mini subscribers lock");
            if let Some(to) = to {
                if let Some(peer) = subscribers.get(to).and_then(Weak::upgrade) { peer.emit_raw(service, payload.clone(), None); }
            } else {
                for peer in subscribers.values().filter_map(Weak::upgrade) { peer.emit_raw(service, payload.clone(), None); }
            }
        }
    });
    let weak_state = Arc::downgrade(state);
    route.worker.peer.on_close(move || {
        if let Some(state) = weak_state.upgrade() { state.routes.lock().expect("mini routes lock").remove(&id); let _ = state.changed.send(()); }
    });
    state.routes.lock().expect("mini routes lock").insert(route.id.clone(), route.clone());
    Ok(route)
}
#[cfg(unix)]
async fn ensure_route(state: &Arc<ServerState>, id: Option<String>, cwd: String) -> Result<Arc<Route>, String> {
    let Some(id) = id else { return spawn_route(state, None, cwd).await; };
    if let Some(route) = state.routes.lock().expect("mini routes lock").get(&id).cloned() { return Ok(route); }
    let lock = state.spawning.lock().expect("mini spawning lock").entry(id.clone()).or_insert_with(|| Arc::new(tokio::sync::Mutex::new(()))).clone();
    let guard = lock.lock().await;
    if let Some(route) = state.routes.lock().expect("mini routes lock").get(&id).cloned() { return Ok(route); }
    let result = spawn_route(state, Some(id.clone()), cwd).await;
    drop(guard);
    state.spawning.lock().expect("mini spawning lock").remove(&id);
    result
}
#[cfg(unix)]
struct Attachment { route: Arc<Route>, id: String }
#[cfg(unix)]
fn presentation(socket: tokio::net::UnixStream, state: &Arc<ServerState>) -> Arc<RpcPeer> {
    let attached = Arc::new(Mutex::new(None::<Attachment>));
    let forward_attached = attached.clone();
    let forward = Arc::new(move |method: String, args: Vec<serde_json::Value>| -> HandlerFuture {
        let route = forward_attached.lock().expect("mini attachment lock").as_ref().map(|attachment| attachment.route.clone());
        Box::pin(async move {
            let route = route.ok_or("Not attached to a session")?;
            let service = method.split_once('.').map_or("", |(name, _)| name);
            if !route.worker.peer.announced().contains(service) { return Err(format!("No host provides {service}: server has [sessions], worker has [{}]", route.worker.peer.announced().into_iter().collect::<Vec<_>>().join(","))); }
            route.worker.peer.call(&method, args).await
        })
    });
    let (input, output) = socket.into_split();
    let peer = Arc::new(create_peer(input, output, PeerOptions { forward: Some(forward), ..Default::default() }));
    let weak_peer = Arc::downgrade(&peer); let weak_state = Arc::downgrade(state); let attach_attached = attached.clone();
    let attach: Handler = Arc::new(move |args, _| {
        let state = weak_state.clone(); let peer = weak_peer.clone(); let attached = attach_attached.clone();
        Box::pin(async move {
            let state = state.upgrade().ok_or("Server closed")?;
            let session_id = args.first().and_then(serde_json::Value::as_str).map(str::to_owned);
            let cwd = args.get(1).and_then(serde_json::Value::as_str).ok_or("Attach requires cwd")?.to_owned();
            let id = args.get(2).and_then(serde_json::Value::as_str).ok_or("Attach requires presentationId")?.to_owned();
            if let Some(old) = attached.lock().expect("mini attachment lock").take() { old.route.subscribers.lock().expect("mini subscribers lock").remove(&old.id); }
            let route = ensure_route(&state, session_id, cwd).await?;
            route.subscribers.lock().expect("mini subscribers lock").insert(id.clone(), peer);
            let session = route.id.clone(); *attached.lock().expect("mini attachment lock") = Some(Attachment { route, id });
            Ok(serde_json::Value::String(session))
        })
    });
    peer.provide(SESSIONS, HashMap::from([("list".to_owned(), sessions_list(state)), ("attach".to_owned(), attach)]));
    let changed = state.changed.clone();
    peer.on_close(move || {
        if let Some(old) = attached.lock().expect("mini attachment lock").take() {
            let mut subscribers = old.route.subscribers.lock().expect("mini subscribers lock"); subscribers.remove(&old.id);
            if subscribers.is_empty() { (old.route.worker.stop)(); }
        }
        let _ = changed.send(());
    });
    peer
}
#[cfg(unix)]
pub async fn run_server(socket_path: &Path, sessions_root: &str, cwd: &str, spawn: SpawnWorker, signal: &maho_ai::utils::abort::AbortSignal, ready: Option<tokio::sync::oneshot::Sender<()>>) -> Result<(), String> {
    let transport = super::shared::transport::SocketTransport { path: socket_path.to_owned() };
    let listener = transport.listen().await.map_err(|error| error.to_string())?;
    struct SocketOwner(PathBuf);
    impl Drop for SocketOwner { fn drop(&mut self) { let _ = std::fs::remove_file(&self.0); } }
    let _owner = SocketOwner(socket_path.to_owned());
    let (changed, mut changes) = tokio::sync::mpsc::unbounded_channel();
    let state = Arc::new(ServerState { routes: Mutex::new(HashMap::new()), spawning: Mutex::new(HashMap::new()), changed, sessions_root: sessions_root.to_owned(), cwd: cwd.to_owned(), spawn });
    let mut peers = Vec::new();
    let mut deadline = Some(tokio::time::Instant::now() + std::time::Duration::from_secs(10));
    if let Some(ready) = ready { let _ = ready.send(()); }
    let result = loop {
        let idle = async { if let Some(deadline) = deadline { tokio::time::sleep_until(deadline).await } else { std::future::pending().await } };
        tokio::select! {
            _ = signal.cancelled() => break Ok(()),
            _ = idle => break Ok(()),
            accepted = listener.accept() => {
                let (socket, _) = match accepted { Ok(accepted) => accepted, Err(error) => break Err(error.to_string()) };
                peers.push(presentation(socket, &state)); deadline = None;
            }
            _ = changes.recv() => {
                peers.retain(|peer| !peer.is_closed());
                deadline = if peers.is_empty() && state.routes.lock().expect("mini routes lock").is_empty() { Some(tokio::time::Instant::now() + std::time::Duration::from_secs(10)) } else { None };
            }
        }
    };
    for peer in peers { peer.close(); }
    for route in state.routes.lock().expect("mini routes lock").values() { (route.worker.stop)(); route.worker.peer.close(); }
    result
}
