use serde_json::json;
use super::shared::{protocol::SessionSummary, rpc::{CallOptions, RpcPeer, create_peer, PeerOptions}, transport::SocketTransport};
#[cfg(unix)]
async fn open_peer(transport: &SocketTransport) -> Result<RpcPeer, String> {
    let socket = tokio::net::UnixStream::connect(&transport.path).await.map_err(|error| error.to_string())?;
    let (input, output) = socket.into_split();
    Ok(create_peer(input, output, PeerOptions::default()))
}
#[cfg(unix)]
pub async fn list_sessions(transport: &SocketTransport) -> Result<Vec<SessionSummary>, String> {
    let peer = open_peer(transport).await?;
    let result = peer.call("sessions.list", Vec::new()).await?;
    serde_json::from_value(result).map_err(|error| error.to_string())
}
pub async fn attach(peer: &RpcPeer, session_id: Option<&str>, cwd: &str, presentation_id: &str) -> Result<String, String> {
    let value = peer.call_with(CallOptions { timeout_ms: Some(60_000), signal: None }, "sessions.attach", vec![json!(session_id), json!(cwd), json!(presentation_id)]).await?;
    serde_json::from_value(value).map_err(|error| error.to_string())
}
