//! Per-route connection step of senpi `experimental/client-runtime.ts` `openClientRuntime`
//! (target :120-169). A Unix route connects over its socket path. A Radius route connects through
//! the Radius relay client transport. After a Radius client connects, a `RadiusClientReconnect`
//! reconnects it and restores its selected session. Disposal stops the reconnector before it
//! disposes the client.
use std::sync::{Arc, Mutex, PoisonError};

use maho_chord::types::JsonValue as Value;
use maho_server::client::{
    Client,
    api::ListenerSubscription,
    errors::ClientError,
    transport::ByteTransportFactory,
    types::{ConnectionState, ConnectionStateChange},
};
use maho_server::protocol::framing::DEFAULT_MAX_FRAME_LENGTH;

use super::radius_auth::RadiusRelayAuthResolver;
use super::radius_relay::{
    RadiusClientReconnect, RadiusReattach, RadiusReconnectClient, RadiusRelayWebSocketFactory,
    create_radius_client_transport_factory,
};

/// senpi `ClientRuntimeRoute`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClientRuntimeRoute {
    #[cfg(unix)]
    Unix(maho_server::client::unix::UnixServerRoute),
    Radius {
        server_id: String,
    },
}
impl ClientRuntimeRoute {
    pub fn server_id(&self) -> &str {
        match self {
            #[cfg(unix)]
            Self::Unix(route) => &route.server_id,
            Self::Radius { server_id } => server_id,
        }
    }
}

/// What a Radius route connects with: senpi `new RadiusRelayAuthResolver(command.auth)` and the
/// WebSocket seam (`None` is the default WebSocket).
#[derive(Clone)]
pub struct RadiusRouteOptions {
    pub auth: Arc<RadiusRelayAuthResolver>,
    pub web_socket_factory: Option<RadiusRelayWebSocketFactory>,
}

/// senpi `route.transport === "unix" ? createUnixTransportFactory({ path }) :
/// createRadiusClientTransportFactory({ serverId, auth })`.
pub fn route_transport_factory(
    route: &ClientRuntimeRoute,
    radius: &RadiusRouteOptions,
) -> Result<Arc<dyn ByteTransportFactory>, ClientError> {
    let factory: Arc<dyn ByteTransportFactory> = match route {
        #[cfg(unix)]
        ClientRuntimeRoute::Unix(route) => Arc::new(
            maho_server::client::unix::UnixTransportFactory::new(route.path.clone(), None)?,
        ) as Arc<dyn ByteTransportFactory>,
        ClientRuntimeRoute::Radius { server_id } => {
            Arc::new(create_radius_client_transport_factory(
                server_id,
                radius.auth.clone(),
                radius.web_socket_factory.clone(),
            )) as Arc<dyn ByteTransportFactory>
        }
    };
    Ok(factory)
}

/// One connected route of the client runtime (senpi `ClientRuntimeServer`'s route and client).
pub struct ClientRuntimeConnection {
    pub route: ClientRuntimeRoute,
    pub client: Arc<Client>,
    reconnect: Option<RadiusRouteReconnect>,
}
impl ClientRuntimeConnection {
    /// Whether a `RadiusClientReconnect` keeps this client connected (Radius routes only).
    pub fn reconnects(&self) -> bool {
        self.reconnect.is_some()
    }
    /// senpi `dispose`: reconnectors first, then the client.
    pub async fn dispose(self) {
        if let Some(reconnect) = &self.reconnect {
            reconnect.dispose().await;
        }
        self.client.dispose();
    }
}

/// senpi `Client.connect({ serverId, transportFactory })` for one route. A Radius route also gets
/// `new RadiusClientReconnect(client, reattach)`; upstream's `reattach` waits for the
/// SessionManagement service and attaches the session, which is the caller's to supply.
pub async fn connect_route(
    route: ClientRuntimeRoute,
    radius: &RadiusRouteOptions,
    reattach: RadiusReattach,
) -> Result<ClientRuntimeConnection, ClientError> {
    let factory = route_transport_factory(&route, radius)?;
    let client = Client::new(
        route.server_id().to_owned(),
        DEFAULT_MAX_FRAME_LENGTH,
        factory,
    )?;
    if let Err(error) = client.connect().await {
        client.dispose();
        return Err(error);
    }
    let reconnect = match &route {
        #[cfg(unix)]
        ClientRuntimeRoute::Unix(_) => None,
        ClientRuntimeRoute::Radius { .. } => {
            match RadiusRouteReconnect::attach(&client, reattach) {
                Ok(reconnect) => Some(reconnect),
                Err(error) => {
                    client.dispose();
                    return Err(error);
                }
            }
        }
    };
    Ok(ClientRuntimeConnection {
        route,
        client,
        reconnect,
    })
}

/// The `Client` surface `RadiusClientReconnect` drives.
struct ReconnectingClient(Arc<Client>);
#[async_trait::async_trait]
impl RadiusReconnectClient for ReconnectingClient {
    fn connected(&self) -> bool {
        self.0.connected()
    }
    fn disconnected(&self) -> bool {
        self.0.connection_state() == ConnectionState::Disconnected
    }
    /// senpi `Client.reconnect()` is `connect()`.
    async fn reconnect(&self) -> Result<(), String> {
        self.0
            .connect()
            .await
            .map(drop)
            .map_err(|error| error.to_string())
    }
    fn disconnect(&self, reason: &str) {
        self.0.disconnect(reason);
    }
}

fn attached_session_id(attachment: &Value) -> Option<String> {
    attachment["sessionId"].as_str().map(str::to_owned)
}

/// `RadiusClientReconnect` with its `onAttachmentChange`/`onConnectionStateChange` client listeners.
struct RadiusRouteReconnect {
    reconnect: Arc<RadiusClientReconnect>,
    listeners: Mutex<Vec<ListenerSubscription>>,
}
impl RadiusRouteReconnect {
    fn attach(client: &Arc<Client>, reattach: RadiusReattach) -> Result<Self, ClientError> {
        let attachment = client.attachment().as_ref().and_then(attached_session_id);
        let reconnect = Arc::new(RadiusClientReconnect::new(
            Arc::new(ReconnectingClient(client.clone())),
            attachment,
            reattach,
        ));
        let on_attachment = reconnect.clone();
        let attachments =
            client.on_attachment_change(Arc::new(move |attachment: Option<Value>| {
                on_attachment.on_attachment_change(
                    attachment.as_ref().and_then(attached_session_id).as_deref(),
                );
            }))?;
        let on_state = reconnect.clone();
        let states =
            client.on_connection_state_change(Arc::new(move |change: ConnectionStateChange| {
                on_state.on_connection_state_change(change.state == ConnectionState::Disconnected);
            }))?;
        Ok(Self {
            reconnect,
            listeners: Mutex::new(vec![attachments, states]),
        })
    }
    /// senpi `RadiusClientReconnect.dispose`: remove both listeners, then stop reconnecting.
    async fn dispose(&self) {
        let listeners = std::mem::take(
            &mut *self
                .listeners
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
        );
        drop(listeners);
        self.reconnect.dispose().await;
    }
}
