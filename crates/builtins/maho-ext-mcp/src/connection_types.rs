use serde::{Serialize,Deserialize};
use crate::errors::McpError;
#[derive(Debug,Clone,Copy,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="snake_case")]
pub enum ServerConnectionState {Disabled,Idle,Connecting,Connected,Degraded,Suspended,NeedsAuth,NeedsClientRegistration}
#[derive(Debug,Clone)]
pub struct ServerConnectionStateChangedEvent {pub server_name:String,pub generation:u64,pub state:ServerConnectionState,pub previous_state:ServerConnectionState,pub error:Option<McpError>}
#[derive(Debug,Clone)]
pub struct ServerConnectionToolsChangedEvent {pub server_name:String,pub generation:u64}
