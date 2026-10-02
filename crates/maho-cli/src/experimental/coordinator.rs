use serde::{Deserialize, Serialize};
#[cfg(unix)]
#[path = "coordinator_runtime.rs"]
mod runtime;
#[cfg(unix)]
pub use runtime::{run_coordinator, run_coordinator_ready};
#[cfg(unix)]
pub struct CoordinatorConnection {
    pub server_connection_id: String,
    pub peer_ids: std::collections::HashSet<String>,
    connection: crate::experimental::mini::shared::transport::SocketConnection,
    replaced: bool,
}
#[cfg(unix)]
impl CoordinatorConnection {
    pub async fn connect(control_path: &std::path::Path, endpoint: &str, server_connection_id: String) -> Result<Self, String> {
        let socket = tokio::net::UnixStream::connect(control_path).await.map_err(|error| error.to_string())?;
        let (input, output) = socket.into_split();
        let mut connection = crate::experimental::mini::shared::transport::JsonConnection::new(input, output);
        connection.send(&serde_json::json!({"type":"register_server", "protocol":COORDINATOR_PROTOCOL_VERSION, "serverConnectionId":server_connection_id, "endpoint":endpoint})).await.map_err(|error| error.to_string())?;
        let message = connection.receive().await.map_err(|error| error.to_string())?.ok_or("Coordinator connection closed")?;
        match serde_json::from_value(message).map_err(|_| "Coordinator sent an invalid message")? {
            CoordinatorMessage::ServerRegistered { server_connection_id: returned, peers } if returned == server_connection_id => Ok(Self { server_connection_id, peer_ids: peers.into_iter().collect(), connection, replaced: false }),
            _ => Err("Coordinator returned an invalid server registration".to_owned()),
        }
    }
    pub fn was_replaced(&self) -> bool { self.replaced }
    pub async fn next_event(&mut self) -> Result<Option<CoordinatorMessage>, String> {
        let Some(value) = self.connection.receive().await.map_err(|error| error.to_string())? else { self.replaced = true; return Ok(None); };
        let message = serde_json::from_value(value).map_err(|_| "Coordinator sent an invalid message")?;
        match &message {
            CoordinatorMessage::PeerConnected { peer_id } => { self.peer_ids.insert(peer_id.clone()); }
            CoordinatorMessage::PeerDisconnected { peer_id } => { self.peer_ids.remove(peer_id); }
            CoordinatorMessage::ServerReplaced => { self.replaced = true; }
            CoordinatorMessage::Message { .. } => {},
            CoordinatorMessage::ServerRegistered { .. } => return Err("Coordinator sent an unsupported message".to_owned()),
        }
        Ok(Some(message))
    }
    pub async fn send(&mut self, peer_id: &str, payload: serde_json::Value) -> Result<(), String> {
        self.write(serde_json::json!({"type":"send", "to":peer_id, "payload":payload})).await
    }
    pub async fn broadcast(&mut self, payload: serde_json::Value) -> Result<(), String> { self.write(serde_json::json!({"type":"broadcast", "payload":payload})).await }
    async fn write(&mut self, message: serde_json::Value) -> Result<(), String> {
        if self.connection.closed() { return Err("Coordinator server is not connected".to_owned()); }
        self.connection.send(&message).await.map_err(|error| error.to_string())
    }
    pub async fn close(&mut self) -> Result<(), String> { self.peer_ids.clear(); self.connection.close().await.map_err(|error| error.to_string()) }
}
pub const COORDINATOR_PROTOCOL_VERSION: u32 = 3;
#[cfg(unix)]
pub struct CoordinatorStartupLease(tokio::net::UnixStream);
#[cfg(unix)]
impl CoordinatorStartupLease { pub fn close(self) { drop(self.0); } }
#[cfg(unix)]
pub async fn ensure_coordinator(public_path: &std::path::Path, control_path: &std::path::Path, cwd: &std::path::Path, env: &std::collections::BTreeMap<String, String>) -> Result<CoordinatorStartupLease, String> {
    async fn try_connect(path: &std::path::Path) -> Result<Option<tokio::net::UnixStream>, String> {
        match tokio::net::UnixStream::connect(path).await {
            Ok(socket) => Ok(Some(socket)),
            Err(error) if matches!(error.kind(), std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused) => Ok(None),
            Err(error) => Err(error.to_string()),
        }
    }
    if let Some(socket) = try_connect(control_path).await? { return Ok(CoordinatorStartupLease(socket)); }
    let args = vec![public_path.to_string_lossy().into_owned(), control_path.to_string_lossy().into_owned()];
    let mut child = crate::experimental::process::spawn_internal_process(crate::experimental::process::InternalProcessRole::Coordinator, &args, cwd, env).map_err(|error| error.to_string())?;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        if let Some(socket) = try_connect(control_path).await? { return Ok(CoordinatorStartupLease(socket)); }
        if child.try_wait().map_err(|error| error.to_string())?.is_some() { return Err("Coordinator exited during startup".to_owned()); }
        if tokio::time::Instant::now() >= deadline { return Err("Timed out waiting for coordinator startup".to_owned()); }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum CoordinatorMessage {
    #[serde(rename = "server_registered")]
    ServerRegistered { #[serde(rename = "serverConnectionId")] server_connection_id: String, peers: Vec<String> },
    #[serde(rename = "server_replaced")]
    ServerReplaced,
    #[serde(rename = "peer_connected")]
    PeerConnected { #[serde(rename = "peerId")] peer_id: String },
    #[serde(rename = "peer_disconnected")]
    PeerDisconnected { #[serde(rename = "peerId")] peer_id: String },
    #[serde(rename = "message")]
    Message { from: String, payload: serde_json::Value },
}
pub fn routed_messages(from: &str, message: &serde_json::Value, peers: &[String], server_present: bool) -> Result<Vec<(String, CoordinatorMessage)>, String> {
    match message["type"].as_str() {
        Some("send") => {
            let target = message["to"].as_str().filter(|target| !target.is_empty()).ok_or("Coordinator message target must be a string")?;
            if (target == "server" && server_present) || peers.iter().any(|peer| peer == target) {
                Ok(vec![(target.to_owned(), CoordinatorMessage::Message { from: from.to_owned(), payload: message["payload"].clone() })])
            } else { Ok(Vec::new()) }
        }
        Some("broadcast") => {
            if from != "server" { return Err("Only the current server may broadcast".to_owned()); }
            Ok(peers.iter().map(|peer| (peer.clone(), CoordinatorMessage::Message { from: from.to_owned(), payload: message["payload"].clone() })).collect())
        }
        kind => Err(format!("Unknown coordinator routing message: {}", kind.unwrap_or("undefined"))),
    }
}
