//! The Radius relay step of senpi `experimental/server.ts` `startServer` (target :656-662). Once the
//! backend is connected and its sessions are refreshed, the backend `Server` is published through a
//! `RadiusRelayHost`. The host accepts each relay client into that server and authenticates with
//! the explicit `relayAuth` credential, or with the stored Radius login when there is none. Upstream
//! closes the relay before it closes the backend; a caller holding the returned host does the same.
use std::sync::Arc;

use super::radius_auth::RadiusRelayAuthResolver;
use super::radius_relay::{
    RadiusRelayHost, RadiusRelayHostOptions, RadiusRelayHostStatus, RadiusRelayWebSocketFactory,
    ServerRelayAcceptor,
};

/// senpi `StartServerOptions.onRelayStatus`.
pub type RelayStatusObserver = Arc<dyn Fn(RadiusRelayHostStatus) + Send + Sync>;

/// The relay inputs of senpi `StartServerOptions`, plus the WebSocket seam.
pub struct ServerRelayOptions {
    pub server_id: String,
    /// senpi `backend.server`.
    pub server: Arc<maho_server::server::Server>,
    /// senpi `new RadiusRelayAuthResolver(options.relayAuth)`.
    pub auth: Arc<RadiusRelayAuthResolver>,
    pub web_socket_factory: Option<RadiusRelayWebSocketFactory>,
    pub on_relay_status: Option<RelayStatusObserver>,
}

/// senpi `relay = new RadiusRelayHost({ serverId, server: backend.server, auth, onStatus }); relay.start()`.
pub fn start_server_relay(options: ServerRelayOptions) -> RadiusRelayHost {
    let relay = RadiusRelayHost::new(RadiusRelayHostOptions {
        server_id: options.server_id,
        server: ServerRelayAcceptor::new(options.server),
        auth: options.auth,
        web_socket_factory: options.web_socket_factory,
        on_status: options.on_relay_status,
    });
    relay.start();
    relay
}
