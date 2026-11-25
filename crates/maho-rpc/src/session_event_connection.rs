//! Port of senpi `packages/coding-agent/src/modes/rpc/session-event-connection.ts`.
//!
//! Connection transport surface and registered connection metadata for `SessionEventWriter`.

use crate::extension_owner::ExtensionTransportOwner;
use crate::socket_closing::SocketClosingCode;
use std::sync::Arc;

#[async_trait::async_trait]
pub trait SessionEventTransport: Send + Sync {
    fn write_raw(&self, chunk: &str) -> Result<(), String>;
    async fn wait_for_backpressure(&self) -> Result<(), String>;
}

pub trait SessionEventWriterConnection: SessionEventTransport {
    fn close(&self);
}

pub type CloseConnectionFn = Arc<dyn Fn(SocketClosingCode, Option<&str>) + Send + Sync>;

#[derive(Clone)]
pub struct RegisteredConnection {
    pub socket_epoch: String,
    pub compact_selected: bool,
    pub owner: Arc<ExtensionTransportOwner>,
    pub close: CloseConnectionFn,
}
