//! senpi `experimental-radius-relay.test.ts` (target 59d58cee): the host bridge, host reconnect and
//! client byte transport cases, over a fake relay WebSocket driven through channels. (The frame and
//! client-reconnect cases live in `radius_relay.rs`.)

use std::sync::{Arc, Mutex};
use std::time::Duration;

use maho_cli::cli::experimental::command_options::AuthInput;
use maho_cli::experimental::radius_auth::{RadiusRelayAuthResolver, StoredRadiusAuth};
use maho_cli::experimental::radius_relay::*;
use maho_server::client::errors::ClientError;
use maho_server::client::transport::{ByteTransport, ByteTransportFactory, ByteTransportHandlers};
use tokio::sync::{mpsc, oneshot};

const SERVER_ID: &str = "00000000-0000-4000-8000-000000000001";
const CONNECTION_ID: &str = "00000000-0000-4000-8000-000000000002";

struct FakeSocket {
    protocol: Mutex<String>,
    sent: Mutex<Vec<RelayFrame>>,
    /// The number of frames sent so far, so a test can await a write instead of polling.
    wrote: tokio::sync::watch::Sender<usize>,
    closed: Mutex<Option<(u16, String)>>,
    events: mpsc::UnboundedSender<RelaySocketEvent>,
    incoming: tokio::sync::Mutex<mpsc::UnboundedReceiver<RelaySocketEvent>>,
}
impl FakeSocket {
    fn emit(&self, event: RelaySocketEvent) {
        let _ = self.events.send(event);
    }
    fn message(&self, frame: RelayFrame) {
        self.emit(RelaySocketEvent::Message(frame));
    }
    fn remote_close(&self) {
        self.emit(RelaySocketEvent::Close {
            code: 1006,
            reason: "lost".into(),
        });
    }
}
#[async_trait::async_trait]
impl RadiusRelayWebSocket for FakeSocket {
    fn protocol(&self) -> String {
        self.protocol.lock().expect("protocol").clone()
    }
    async fn send(&self, frame: RelayFrame) -> Result<(), String> {
        let count = {
            let mut sent = self.sent.lock().expect("sent");
            sent.push(frame);
            sent.len()
        };
        self.wrote.send_replace(count);
        Ok(())
    }
    fn close(&self, code: u16, reason: &str) {
        let mut closed = self.closed.lock().expect("closed");
        if closed.is_none() {
            *closed = Some((code, reason.to_owned()));
            self.emit(RelaySocketEvent::Close {
                code,
                reason: reason.to_owned(),
            });
        }
    }
    async fn next_event(&self) -> Option<RelaySocketEvent> {
        self.incoming.lock().await.recv().await
    }
}

/// One factory call: the socket, the request, and the opener (`Ok(protocol)` or the error).
type Opening = (
    Arc<FakeSocket>,
    RelaySocketRequest,
    oneshot::Sender<Result<String, String>>,
);

fn socket_factory() -> (
    RadiusRelayWebSocketFactory,
    mpsc::UnboundedReceiver<Opening>,
) {
    let (created, openings) = mpsc::unbounded_channel::<Opening>();
    let factory: RadiusRelayWebSocketFactory = Arc::new(
        move |request: RelaySocketRequest| -> futures::future::BoxFuture<
            'static,
            Result<Arc<dyn RadiusRelayWebSocket>, String>,
        > {
            let (events, incoming) = mpsc::unbounded_channel();
            let socket = Arc::new(FakeSocket {
                protocol: Mutex::default(),
                sent: Mutex::default(),
                wrote: tokio::sync::watch::Sender::new(0),
                closed: Mutex::default(),
                events,
                incoming: tokio::sync::Mutex::new(incoming),
            });
            let (open, opened) = oneshot::channel();
            let _ = created.send((socket.clone(), request, open));
            Box::pin(async move {
                let protocol = opened.await.map_err(|_| "factory dropped".to_owned())??;
                *socket.protocol.lock().expect("protocol") = protocol;
                Ok(socket as Arc<dyn RadiusRelayWebSocket>)
            })
        },
    );
    (factory, openings)
}

fn token_auth() -> Arc<RadiusRelayAuthResolver> {
    let stored: StoredRadiusAuth = Arc::new(
        |_| -> futures::future::BoxFuture<
            'static,
            Result<Option<maho_ai::models::AuthResolution>, String>,
        > { Box::pin(std::future::ready(Ok(None))) },
    );
    Arc::new(RadiusRelayAuthResolver::with_parts(
        Some(AuthInput::Token("secret".into())),
        "https://radius.example.org",
        "/",
        false,
        stored,
    ))
}

async fn within<T>(what: &str, future: impl std::future::Future<Output = Option<T>>) -> T {
    tokio::time::timeout(Duration::from_secs(5), future)
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
        .unwrap_or_else(|| panic!("{what}: channel closed"))
}

fn statuses() -> (
    Arc<dyn Fn(RadiusRelayHostStatus) + Send + Sync>,
    mpsc::UnboundedReceiver<RadiusRelayHostStatus>,
) {
    let (sender, receiver) = mpsc::unbounded_channel();
    (
        Arc::new(move |status: RadiusRelayHostStatus| drop(sender.send(status))),
        receiver,
    )
}

async fn until_connected(statuses: &mut mpsc::UnboundedReceiver<RadiusRelayHostStatus>) {
    while within("the host status", statuses.recv()).await != RadiusRelayHostStatus::Connected {}
}

struct RecordingHandler {
    data: mpsc::UnboundedSender<Vec<u8>>,
    closed: mpsc::UnboundedSender<()>,
}
impl RelayByteConnectionHandler for RecordingHandler {
    fn on_data(&self, chunk: &[u8]) {
        let _ = self.data.send(chunk.to_vec());
    }
    fn on_close(&self) {
        let _ = self.closed.send(());
    }
    fn on_error(&self, error: &str) {
        panic!("unexpected connection error {error}");
    }
}
struct RecordingServer {
    accepted: mpsc::UnboundedSender<Arc<RelayServerByteConnection>>,
    handler: Arc<RecordingHandler>,
}
impl RelayServer for RecordingServer {
    fn accept(
        &self,
        connection: Arc<RelayServerByteConnection>,
    ) -> Arc<dyn RelayByteConnectionHandler> {
        let _ = self.accepted.send(connection);
        self.handler.clone()
    }
}

#[tokio::test]
async fn bridges_multiplexed_host_clients_into_independent_server_connections() {
    let (factory, mut openings) = socket_factory();
    let (accepted, mut accepts) = mpsc::unbounded_channel();
    let (data, mut received) = mpsc::unbounded_channel();
    let (closed, mut closes) = mpsc::unbounded_channel();
    let (on_status, mut status) = statuses();
    let host = RadiusRelayHost::new(RadiusRelayHostOptions {
        server_id: SERVER_ID.into(),
        server: Arc::new(RecordingServer {
            accepted,
            handler: Arc::new(RecordingHandler { data, closed }),
        }),
        auth: token_auth(),
        web_socket_factory: Some(factory),
        on_status: Some(on_status),
    });
    host.start();
    let (socket, request, open) = within("the host socket", openings.recv()).await;
    assert_eq!(request.authorization, "Bearer secret");
    assert_eq!(request.protocol, RADIUS_RELAY_HOST_SUBPROTOCOL);
    assert_eq!(
        request.url,
        format!("wss://radius.example.org/v1/session-relays/{SERVER_ID}/connect")
    );
    open.send(Ok(request.protocol.clone())).expect("open");
    until_connected(&mut status).await;

    socket.message(RelayFrame::Text(
        serde_json::json!({"version": 1, "type": "connection_open", "connection_id": CONNECTION_ID}).to_string(),
    ));
    let connection = within("the accepted connection", accepts.recv()).await;
    socket.message(RelayFrame::Binary(
        encode_relay_data_frame(CONNECTION_ID, &[1, 2, 3]).expect("frame"),
    ));
    assert_eq!(within("client data", received.recv()).await, [1, 2, 3]);

    connection.send(&[4, 5, 6]).await.expect("send to client");
    let last = socket.sent.lock().expect("sent").last().cloned();
    let Some(RelayFrame::Binary(outbound)) = last.clone() else {
        panic!("expected a binary frame, got {last:?}")
    };
    assert_eq!(
        parse_relay_data_frame(&outbound),
        Some((CONNECTION_ID.to_owned(), vec![4, 5, 6]))
    );

    socket.message(RelayFrame::Text(
        serde_json::json!({"version": 1, "type": "connection_close", "connection_id": CONNECTION_ID}).to_string(),
    ));
    within("the connection close", closes.recv()).await;
    assert!(connection.closed());
    assert!(closes.try_recv().is_err(), "closed once");
    host.close().await;
}

struct NoServer;
impl RelayServer for NoServer {
    fn accept(&self, _: Arc<RelayServerByteConnection>) -> Arc<dyn RelayByteConnectionHandler> {
        panic!("no connection is opened")
    }
}

#[tokio::test(start_paused = true)]
async fn reconnects_the_server_host_after_the_relay_connection_drops() {
    let (factory, mut openings) = socket_factory();
    let (on_status, mut status) = statuses();
    let host = RadiusRelayHost::new(RadiusRelayHostOptions {
        server_id: SERVER_ID.into(),
        server: Arc::new(NoServer),
        auth: token_auth(),
        web_socket_factory: Some(factory),
        on_status: Some(on_status),
    });
    host.start();
    let (socket, request, open) = within("the first host socket", openings.recv()).await;
    open.send(Ok(request.protocol)).expect("open");
    until_connected(&mut status).await;

    let dropped = tokio::time::Instant::now();
    socket.remote_close();
    assert_eq!(
        within("the retry status", status.recv()).await,
        RadiusRelayHostStatus::Retrying {
            error: "Radius relay host closed (1006: lost)".into()
        }
    );
    let (_second, _, _open) = within("the second host socket", openings.recv()).await;
    assert_eq!(dropped.elapsed(), Duration::from_millis(1_000));
    host.close().await;
}

fn handlers() -> (
    ByteTransportHandlers,
    mpsc::UnboundedReceiver<Vec<u8>>,
    mpsc::UnboundedReceiver<()>,
    mpsc::UnboundedReceiver<ClientError>,
) {
    let (data, on_data) = mpsc::unbounded_channel();
    let (close, on_close) = mpsc::unbounded_channel();
    let (error, on_error) = mpsc::unbounded_channel();
    (
        ByteTransportHandlers {
            on_data: Arc::new(move |chunk: &[u8]| drop(data.send(chunk.to_vec()))),
            on_close: Arc::new(move || drop(close.send(()))),
            on_error: Arc::new(move |failure: ClientError| drop(error.send(failure))),
        },
        on_data,
        on_close,
        on_error,
    )
}

#[tokio::test]
async fn uses_a_raw_authenticated_websocket_as_the_client_byte_transport() {
    let (factory, mut openings) = socket_factory();
    let transports = create_radius_client_transport_factory(SERVER_ID, token_auth(), Some(factory));
    let (handlers, mut data, mut close, mut error) = handlers();
    let (transport, socket) = tokio::join!(transports.connect(handlers), async {
        let (socket, request, open) = within("the client socket", openings.recv()).await;
        assert_eq!(request.authorization, "Bearer secret");
        assert_eq!(request.protocol, RADIUS_RELAY_CLIENT_SUBPROTOCOL);
        open.send(Ok(request.protocol)).expect("open");
        socket
    });
    let transport = transport.expect("transport");

    transport.send(&[1, 2, 3]).await.expect("send");
    assert_eq!(
        socket.sent.lock().expect("sent").first(),
        Some(&RelayFrame::Binary(vec![1, 2, 3]))
    );
    socket.message(RelayFrame::Binary(vec![4, 5, 6]));
    assert_eq!(within("client data", data.recv()).await, [4, 5, 6]);
    socket.remote_close();
    within("the close", close.recv()).await;
    assert!(error.try_recv().is_err());
}

#[tokio::test]
async fn reports_established_abnormal_closures_so_the_client_can_reconnect() {
    let (factory, mut openings) = socket_factory();
    let transports = create_radius_client_transport_factory(SERVER_ID, token_auth(), Some(factory));
    let (handlers, _data, mut close, mut error) = handlers();
    let (transport, socket) = tokio::join!(transports.connect(handlers), async {
        let (socket, request, open) = within("the client socket", openings.recv()).await;
        open.send(Ok(request.protocol)).expect("open");
        socket
    });
    let _transport = transport.expect("transport");

    socket.emit(RelaySocketEvent::Error("network lost".into()));
    socket.remote_close();
    assert_eq!(
        within("the error", error.recv()).await.to_string(),
        "network lost"
    );
    assert!(error.try_recv().is_err(), "reported once");
    assert!(close.try_recv().is_err(), "an error is not also a close");
}

#[tokio::test]
async fn reports_a_useful_error_when_the_websocket_omits_failure_details() {
    let (factory, mut openings) = socket_factory();
    let transports = create_radius_client_transport_factory(SERVER_ID, token_auth(), Some(factory));
    let (handlers, ..) = handlers();
    let (opened, ()) = tokio::join!(transports.connect(handlers), async {
        let (_socket, _request, open) = within("the client socket", openings.recv()).await;
        open.send(Err(String::new())).expect("fail");
    });
    let Err(failure) = opened else {
        panic!("the connection must fail")
    };
    assert_eq!(failure.to_string(), "Radius WebSocket connection failed");
}

async fn until_written(socket: &FakeSocket, count: usize) {
    let mut wrote = socket.wrote.subscribe();
    tokio::time::timeout(
        Duration::from_secs(5),
        wrote.wait_for(|sent| *sent >= count),
    )
    .await
    .expect("timed out waiting for a socket write")
    .expect("socket dropped");
}

fn sent_frame(socket: &FakeSocket, index: usize) -> Vec<u8> {
    match socket.sent.lock().expect("sent").get(index).cloned() {
        Some(RelayFrame::Binary(bytes)) => bytes,
        other => panic!("expected binary frame {index}, got {other:?}"),
    }
}

/// senpi `startServer` (server.ts:656): the backend `Server` is what the relay host accepts relay
/// clients into, so a relay client completes the real server handshake.
#[tokio::test]
async fn start_server_relay_publishes_the_backend_server_through_the_radius_relay() {
    use maho_cli::experimental::server_relay::{ServerRelayOptions, start_server_relay};
    use maho_server::protocol::{
        codec::{MessageDecoder, encode_client_message},
        framing::DEFAULT_MAX_FRAME_LENGTH,
    };
    let backend = maho_server::server::testing::create_test_server(Default::default())
        .expect("backend server");
    let (factory, mut openings) = socket_factory();
    let (on_status, mut status) = statuses();
    let relay = start_server_relay(ServerRelayOptions {
        server_id: SERVER_ID.into(),
        server: backend.server.clone(),
        auth: token_auth(),
        web_socket_factory: Some(factory),
        on_relay_status: Some(on_status),
    });
    let (socket, request, open) = within("the host socket", openings.recv()).await;
    assert_eq!(request.protocol, RADIUS_RELAY_HOST_SUBPROTOCOL);
    assert_eq!(
        request.url,
        format!("wss://radius.example.org/v1/session-relays/{SERVER_ID}/connect")
    );
    open.send(Ok(request.protocol.clone())).expect("open");
    until_connected(&mut status).await;

    socket.message(RelayFrame::Text(
        serde_json::json!({"version": 1, "type": "connection_open", "connection_id": CONNECTION_ID}).to_string(),
    ));
    let hello = encode_client_message(
        &maho_server::transport_json!({"type": "hello", "version": 8}),
        DEFAULT_MAX_FRAME_LENGTH,
    )
    .expect("client hello");
    socket.message(RelayFrame::Binary(
        encode_relay_data_frame(CONNECTION_ID, &hello).expect("frame"),
    ));
    until_written(&socket, 1).await;
    let (connection_id, payload) =
        parse_relay_data_frame(&sent_frame(&socket, 0)).expect("relay data frame");
    assert_eq!(connection_id, CONNECTION_ID);
    let messages = MessageDecoder::server()
        .push(&payload)
        .expect("server frame");
    assert_eq!(messages[0]["type"].as_str(), Some("hello"));
    assert_eq!(messages[0]["serverId"].as_str(), Some(SERVER_ID));
    relay.close().await;
}

/// Opens the next client relay socket and answers the client hello with `hello`.
async fn answer_client(
    openings: &mut mpsc::UnboundedReceiver<Opening>,
    hello: &[u8],
) -> Arc<FakeSocket> {
    let (socket, request, open) = within("the client socket", openings.recv()).await;
    assert_eq!(request.protocol, RADIUS_RELAY_CLIENT_SUBPROTOCOL);
    assert_eq!(request.authorization, "Bearer secret");
    open.send(Ok(request.protocol)).expect("open");
    until_written(&socket, 1).await;
    socket.message(RelayFrame::Binary(hello.to_vec()));
    socket
}

/// senpi `openClientRuntime` (client-runtime.ts:129): a Radius route connects through the relay
/// client transport and is kept connected by `RadiusClientReconnect`, which restores the session.
#[tokio::test(start_paused = true)]
async fn a_radius_route_connects_over_the_relay_and_reattaches_after_a_drop() {
    use maho_cli::experimental::client_runtime::{
        ClientRuntimeRoute, RadiusRouteOptions, connect_route,
    };
    use maho_server::protocol::{codec::encode_server_message, framing::DEFAULT_MAX_FRAME_LENGTH};
    let (factory, mut openings) = socket_factory();
    let (reattached, mut reattaches) = mpsc::unbounded_channel::<String>();
    let reattach: RadiusReattach = Arc::new(move |session_id| {
        let _ = reattached.send(session_id);
        Box::pin(std::future::ready(Ok(())))
    });
    let radius = RadiusRouteOptions {
        auth: token_auth(),
        web_socket_factory: Some(factory),
    };
    let server_hello = encode_server_message(
        &maho_server::transport_json!({"type": "hello", "version": 8, "serverId": SERVER_ID}),
        DEFAULT_MAX_FRAME_LENGTH,
    )
    .expect("server hello");
    let (connection, socket) = tokio::join!(
        connect_route(
            ClientRuntimeRoute::Radius {
                server_id: SERVER_ID.into()
            },
            &radius,
            reattach
        ),
        answer_client(&mut openings, &server_hello),
    );
    let connection = connection.expect("radius route");
    assert!(connection.client.connected());
    assert!(connection.reconnects());

    let attachment = encode_server_message(
        &maho_server::transport_json!({"type": "attachment", "attachment": {"serverId": SERVER_ID, "sessionId": "session-1", "attachmentId": "attachment-1"}}),
        DEFAULT_MAX_FRAME_LENGTH,
    )
    .expect("attachment");
    let mut changes = connection.client.attachment_changes();
    socket.message(RelayFrame::Binary(attachment));
    tokio::time::timeout(Duration::from_secs(5), changes.wait_for(Option::is_some))
        .await
        .expect("attachment timeout")
        .expect("attachment");

    socket.remote_close();
    let _second = answer_client(&mut openings, &server_hello).await;
    assert_eq!(within("the reattach", reattaches.recv()).await, "session-1");
    assert!(connection.client.connected());
    connection.dispose().await;
}
