use serde::{Deserialize, Serialize};
pub const COORDINATOR_PROTOCOL_VERSION: u32 = 3;
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
