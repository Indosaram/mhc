use super::errors::ClientError;
use serde_json::Value;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectionState {
    Disconnected,
    Connecting,
    Connected,
}

#[derive(Clone, Debug)]
pub struct ConnectionStateChange {
    pub state: ConnectionState,
    pub error: Option<ClientError>,
}

pub type HandshakeHandler = Arc<dyn Fn(&Value) -> Result<(), ClientError> + Send + Sync>;
pub struct ConnectionOptions {
    pub server_id: String,
    pub max_frame_length: u32,
    pub on_handshake: HandshakeHandler,
    pub on_message: Arc<dyn Fn(Value) -> Result<(), ClientError> + Send + Sync>,
    pub on_state_change: Arc<dyn Fn(ConnectionStateChange) + Send + Sync>,
}
