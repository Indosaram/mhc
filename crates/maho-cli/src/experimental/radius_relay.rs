use std::sync::LazyLock;
pub const RADIUS_RELAY_HOST_SUBPROTOCOL: &str = "pi-session-relay.host.v1";
pub const RADIUS_RELAY_CLIENT_SUBPROTOCOL: &str = "pi-session-relay.client.v1";
fn valid_connection_id(id: &str) -> bool {
    static PATTERN: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$").expect("connection ID regex"));
    PATTERN.is_match(id)
}
pub fn encode_relay_data_frame(connection_id: &str, payload: &[u8]) -> Result<Vec<u8>, &'static str> {
    if !valid_connection_id(connection_id) { return Err("Invalid Radius relay connection ID"); }
    let hex = connection_id.replace('-', "");
    let mut frame = vec![1, 1];
    for index in 0..16 { frame.push(u8::from_str_radix(&hex[index * 2..index * 2 + 2], 16).expect("validated hexadecimal UUID")); }
    frame.extend_from_slice(payload);
    Ok(frame)
}
pub fn parse_relay_data_frame(frame: &[u8]) -> Option<(String, Vec<u8>)> {
    if frame.len() < 18 || frame[..2] != [1, 1] { return None; }
    let mut hex = String::new();
    use std::fmt::Write;
    for byte in &frame[2..18] { write!(hex, "{byte:02x}").expect("write to String"); }
    let id = format!("{}-{}-{}-{}-{}", &hex[..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..]);
    valid_connection_id(&id).then(|| (id, frame[18..].to_vec()))
}
pub fn relay_websocket_url(gateway: &str, server_id: &str) -> Result<String, String> {
    let mut url = url::Url::parse(gateway).and_then(|url| url.join(&format!("/v1/session-relays/{server_id}/connect"))).map_err(|error| error.to_string())?;
    let scheme = match url.scheme() { "https" => "wss", "http" => "ws", scheme => return Err(format!("Unsupported Radius gateway protocol: {scheme}:")) };
    url.set_scheme(scheme).map_err(|()| "Unable to set WebSocket scheme".to_owned())?;
    Ok(url.to_string())
}
/// senpi `HOST_RETRY_INITIAL_MS`/`HOST_RETRY_MAX_MS`/`MISSING_AUTH_RETRY_MS`/`CLIENT_RETRY_*`.
pub const HOST_RETRY_INITIAL_MS: u64 = 1_000;
pub const HOST_RETRY_MAX_MS: u64 = 30_000;
pub const MISSING_AUTH_RETRY_MS: u64 = 30_000;
pub const CLIENT_RETRY_INITIAL_MS: u64 = 1_000;
pub const CLIENT_RETRY_MAX_MS: u64 = 30_000;
/// senpi `LOCAL_PROTOCOL_ERROR_CLOSE_CODE`/`LOCAL_TRANSPORT_ERROR_CLOSE_CODE`: application close codes a WebSocket client may send.
pub const LOCAL_PROTOCOL_ERROR_CLOSE_CODE: u16 = 4000;
pub const LOCAL_TRANSPORT_ERROR_CLOSE_CODE: u16 = 4001;
/// senpi `RadiusRelayHostStatus`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RadiusRelayHostStatus { NotAuthenticated, Connecting, Connected, Retrying { error: String } }
/// senpi `HostInputControlMessage`: what the relay sends the host as text frames.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HostControlMessage { Ping, Pong, ConnectionOpen { connection_id: String }, ConnectionClose { connection_id: String, code: Option<u16> } }
/// senpi `parseHostControlMessage`.
pub fn parse_host_control_message(value: &str) -> Result<HostControlMessage, String> {
    let invalid = || "Invalid Radius relay control message".to_owned();
    let parsed: serde_json::Value = serde_json::from_str(value).map_err(|_| invalid())?;
    let message = parsed.as_object().ok_or_else(invalid)?;
    if message.get("version").and_then(serde_json::Value::as_f64) != Some(1.0) { return Err("Unsupported Radius relay control version".to_owned()); }
    let kind = message.get("type").and_then(serde_json::Value::as_str);
    match kind {
        Some("ping") => return Ok(HostControlMessage::Ping),
        Some("pong") => return Ok(HostControlMessage::Pong),
        _ => {}
    }
    let connection_id = message.get("connection_id").and_then(serde_json::Value::as_str).filter(|id| valid_connection_id(id)).ok_or_else(invalid)?.to_owned();
    let code = match message.get("code") {
        None => None,
        Some(code) => Some(code.as_f64().filter(|code| code.fract() == 0.0 && (1000.0..=4999.0).contains(code)).map(|code| code as u16).ok_or_else(invalid)?),
    };
    match kind {
        Some("connection_open") => Ok(HostControlMessage::ConnectionOpen { connection_id }),
        Some("connection_close") => Ok(HostControlMessage::ConnectionClose { connection_id, code }),
        _ => Err(invalid()),
    }
}
/// The `Client` surface `RadiusClientReconnect` drives (senpi `RadiusReconnectClient`).
#[async_trait::async_trait]
pub trait RadiusReconnectClient: Send + Sync {
    fn connected(&self) -> bool;
    /// senpi `connectionState === "disconnected"`.
    fn disconnected(&self) -> bool;
    async fn reconnect(&self) -> Result<(), String>;
    fn disconnect(&self, reason: &str);
}
pub type RadiusReattach = std::sync::Arc<dyn Fn(String) -> futures::future::BoxFuture<'static, Result<(), String>> + Send + Sync>;
struct ReconnectInner {
    client: std::sync::Arc<dyn RadiusReconnectClient>,
    reattach: RadiusReattach,
    abort: maho_ai::utils::abort::AbortController,
    desired_session_id: std::sync::Mutex<Option<String>>,
    reconnecting: std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
    disposed: std::sync::atomic::AtomicBool,
}
/// senpi `RadiusClientReconnect`: reconnects an established client with exponential backoff (1 s doubling to 30 s) and
/// restores its selected session. The client's listeners are the `on_attachment_change`/`on_connection_state_change` calls.
pub struct RadiusClientReconnect { inner: std::sync::Arc<ReconnectInner> }
impl RadiusClientReconnect {
    pub fn new(client: std::sync::Arc<dyn RadiusReconnectClient>, attachment: Option<String>, reattach: RadiusReattach) -> Self {
        Self { inner: std::sync::Arc::new(ReconnectInner { client, reattach, abort: maho_ai::utils::abort::AbortController::new(), desired_session_id: std::sync::Mutex::new(attachment), reconnecting: std::sync::Mutex::new(None), disposed: std::sync::atomic::AtomicBool::new(false) }) }
    }
    fn disposed(&self) -> bool { self.inner.disposed.load(std::sync::atomic::Ordering::SeqCst) }
    /// senpi `onAttachmentChange`: a new attachment is remembered; a detach while connected forgets it.
    pub fn on_attachment_change(&self, session_id: Option<&str>) {
        let mut desired = self.inner.desired_session_id.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        match session_id {
            Some(id) => *desired = Some(id.to_owned()),
            None if self.inner.client.connected() => *desired = None,
            None => {}
        }
    }
    /// senpi `onConnectionStateChange`: a disconnect starts one reconnect loop.
    pub fn on_connection_state_change(&self, disconnected: bool) {
        if !disconnected || self.disposed() { return; }
        let mut reconnecting = self.inner.reconnecting.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if reconnecting.as_ref().is_some_and(|task| !task.is_finished()) { return; }
        *reconnecting = Some(tokio::spawn(run_reconnect(self.inner.clone())));
    }
    /// senpi `dispose`.
    pub async fn dispose(&self) {
        if self.inner.disposed.swap(true, std::sync::atomic::Ordering::SeqCst) { return; }
        self.inner.abort.abort(None);
        if !self.inner.client.disconnected() { self.inner.client.disconnect("Radius reconnect stopped"); }
        let task = self.inner.reconnecting.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take();
        if let Some(task) = task { let _ = task.await; }
    }
}
async fn run_reconnect(inner: std::sync::Arc<ReconnectInner>) {
    let signal = inner.abort.signal();
    let disposed = || inner.disposed.load(std::sync::atomic::Ordering::SeqCst);
    let mut retry_ms = CLIENT_RETRY_INITIAL_MS;
    while !disposed() && !inner.client.connected() {
        tokio::select! {
            () = signal.cancelled() => return,
            () = tokio::time::sleep(std::time::Duration::from_millis(retry_ms)) => {}
        }
        let attempt = async {
            inner.client.reconnect().await?;
            let session_id = inner.desired_session_id.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
            if let Some(session_id) = session_id { (inner.reattach)(session_id).await?; }
            Ok::<(), String>(())
        };
        match attempt.await {
            Ok(()) => return,
            Err(error) => {
                if disposed() || signal.aborted() { return; }
                if inner.client.connected() { inner.client.disconnect(&error); }
                retry_ms = (retry_ms * 2).min(CLIENT_RETRY_MAX_MS);
            }
        }
    }
}

// --- WebSocket seam (senpi `RadiusRelayWebSocket` / `RadiusRelayWebSocketFactory`) -------------

/// One WebSocket data message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RelayFrame { Text(String), Binary(Vec<u8>) }
/// What an open relay WebSocket reports: senpi's `message`, `close` and `error` events.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RelaySocketEvent { Message(RelayFrame), Close { code: u16, reason: String }, Error(String) }
/// senpi `RadiusRelayWebSocket` after its `open` event.
#[async_trait::async_trait]
pub trait RadiusRelayWebSocket: Send + Sync {
    /// The negotiated subprotocol.
    fn protocol(&self) -> String;
    async fn send(&self, frame: RelayFrame) -> Result<(), String>;
    /// senpi `close(code, reason)`; never fails (a pending handshake is simply dropped).
    fn close(&self, code: u16, reason: &str);
    /// The next event; `None` once the socket has nothing more to report.
    async fn next_event(&self) -> Option<RelaySocketEvent>;
}
/// senpi's factory options: `{ url, protocol, authorization }`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RelaySocketRequest { pub url: String, pub protocol: String, pub authorization: String }
/// senpi `RadiusRelayWebSocketFactory`, resolving once the socket is open. An empty error message is
/// reported as "Radius WebSocket connection failed" (senpi `webSocketError`).
pub type RadiusRelayWebSocketFactory = std::sync::Arc<dyn Fn(RelaySocketRequest) -> futures::future::BoxFuture<'static, Result<std::sync::Arc<dyn RadiusRelayWebSocket>, String>> + Send + Sync>;

/// senpi `MAX_PENDING_BYTES`: four maximum protocol frames.
pub const MAX_PENDING_BYTES: usize = maho_server::protocol::framing::DEFAULT_MAX_FRAME_LENGTH as usize * 4;

fn web_socket_error(message: &str) -> String {
    let message = message.trim();
    if message.is_empty() { "Radius WebSocket connection failed".to_owned() } else { message.to_owned() }
}

/// senpi `delay(ms, signal)`: false when the signal aborted first.
async fn delay(milliseconds: u64, signal: &maho_ai::utils::abort::AbortSignal) -> bool {
    if signal.aborted() { return false; }
    tokio::select! {
        () = signal.cancelled() => false,
        () = tokio::time::sleep(std::time::Duration::from_millis(milliseconds)) => true,
    }
}

/// senpi `OrderedWebSocketWriter`: writes in call order, bounded by `MAX_PENDING_BYTES`. The
/// awaited send provides the backpressure senpi polls `bufferedAmount` for.
struct OrderedWebSocketWriter {
    socket: std::sync::Arc<dyn RadiusRelayWebSocket>,
    line: tokio::sync::Mutex<()>,
    pending: std::sync::atomic::AtomicUsize,
    closed: std::sync::atomic::AtomicBool,
}
impl OrderedWebSocketWriter {
    fn new(socket: std::sync::Arc<dyn RadiusRelayWebSocket>) -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self { socket, line: tokio::sync::Mutex::new(()), pending: std::sync::atomic::AtomicUsize::new(0), closed: std::sync::atomic::AtomicBool::new(false) })
    }
    fn is_closed(&self) -> bool { self.closed.load(std::sync::atomic::Ordering::SeqCst) }
    async fn send(&self, frame: RelayFrame) -> Result<(), String> {
        use std::sync::atomic::Ordering;
        if self.is_closed() { return Err("Radius relay WebSocket is closed".to_owned()); }
        let length = match &frame { RelayFrame::Text(text) => text.len(), RelayFrame::Binary(bytes) => bytes.len() };
        self.pending
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |pending| pending.checked_add(length).filter(|sum| *sum <= MAX_PENDING_BYTES))
            .map_err(|_| "Radius relay exceeded its pending byte limit".to_owned())?;
        let result = async {
            let _line = self.line.lock().await;
            if self.is_closed() { return Err("Radius relay WebSocket is closed".to_owned()); }
            self.socket.send(frame).await
        }
        .await;
        self.pending.fetch_sub(length, Ordering::SeqCst);
        result
    }
    fn close(&self) { self.closed.store(true, std::sync::atomic::Ordering::SeqCst); }
}

/// senpi `openRadiusRelayWebSocket`: opens with `Bearer <token>`, checks the negotiated subprotocol,
/// and gives up when `signal` aborts first.
async fn open_radius_relay_websocket(url: String, protocol: &str, token: &str, factory: &RadiusRelayWebSocketFactory, signal: Option<&maho_ai::utils::abort::AbortSignal>) -> Result<std::sync::Arc<dyn RadiusRelayWebSocket>, String> {
    if signal.is_some_and(maho_ai::utils::abort::AbortSignal::aborted) { return Err("The operation was aborted".to_owned()); }
    let opening = factory(RelaySocketRequest { url, protocol: protocol.to_owned(), authorization: format!("Bearer {token}") });
    let opened = match signal {
        Some(signal) => tokio::select! {
            () = signal.cancelled() => return Err("Radius relay connection cancelled".to_owned()),
            opened = opening => opened,
        },
        None => opening.await,
    };
    let socket = opened.map_err(|error| web_socket_error(&error))?;
    let selected = socket.protocol();
    if selected != protocol {
        socket.close(1000, "Radius relay connection failed");
        return Err(format!("Radius relay selected unexpected WebSocket protocol {}", serde_json::Value::String(selected)));
    }
    Ok(socket)
}

// --- Host (senpi `RadiusRelayHost`) ------------------------------------------------------------

/// senpi `RelayByteConnectionHandler`: what `server.accept(connection)` returns.
pub trait RelayByteConnectionHandler: Send + Sync {
    fn on_data(&self, chunk: &[u8]);
    fn on_close(&self);
    fn on_error(&self, error: &str);
}
/// senpi `Pick<Server, "accept">`.
pub trait RelayServer: Send + Sync {
    fn accept(&self, connection: std::sync::Arc<RelayServerByteConnection>) -> std::sync::Arc<dyn RelayByteConnectionHandler>;
}

type ActiveConnection = (std::sync::Arc<RelayServerByteConnection>, std::sync::Arc<dyn RelayByteConnectionHandler>);
struct HostShared {
    writer: std::sync::Mutex<Option<std::sync::Arc<OrderedWebSocketWriter>>>,
    connections: std::sync::Mutex<std::collections::HashMap<String, ActiveConnection>>,
}
fn lock<T>(mutex: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> { mutex.lock().unwrap_or_else(std::sync::PoisonError::into_inner) }
impl HostShared {
    fn require_writer(&self) -> Result<std::sync::Arc<OrderedWebSocketWriter>, String> {
        lock(&self.writer).clone().ok_or_else(|| "Radius relay host is disconnected".to_owned())
    }
    async fn send_control(&self, message: serde_json::Value) -> Result<(), String> {
        self.require_writer()?.send(RelayFrame::Text(message.to_string())).await
    }
    async fn send_data(&self, connection_id: &str, chunk: &[u8]) -> Result<(), String> {
        if !lock(&self.connections).contains_key(connection_id) { return Err("Radius relay connection is closed".to_owned()); }
        let frame = encode_relay_data_frame(connection_id, chunk).map_err(str::to_owned)?;
        self.require_writer()?.send(RelayFrame::Binary(frame)).await
    }
    async fn server_close_connection(&self, connection_id: &str, final_chunk: Option<Vec<u8>>) -> Result<(), String> {
        if lock(&self.connections).remove(connection_id).is_none() { return Ok(()); }
        if let Some(chunk) = final_chunk {
            let frame = encode_relay_data_frame(connection_id, &chunk).map_err(str::to_owned)?;
            self.require_writer()?.send(RelayFrame::Binary(frame)).await?;
        }
        self.send_control(serde_json::json!({"version": 1, "type": "connection_close", "connection_id": connection_id, "code": 1000})).await
    }
    fn drop_connections(&self, error: Option<&str>) {
        let connections: Vec<_> = lock(&self.connections).drain().map(|(_, active)| active).collect();
        for (connection, handler) in connections {
            connection.mark_closed();
            match error { Some(error) => handler.on_error(error), None => handler.on_close() }
        }
    }
}

/// senpi `RelayServerByteConnection`: one multiplexed relay client as the server sees it.
pub struct RelayServerByteConnection {
    connection_id: String,
    host: std::sync::Weak<HostShared>,
    closed: std::sync::atomic::AtomicBool,
}
impl RelayServerByteConnection {
    pub fn closed(&self) -> bool { self.closed.load(std::sync::atomic::Ordering::SeqCst) }
    pub async fn send(&self, chunk: &[u8]) -> Result<(), String> {
        if self.closed() { return Err("Radius relay connection is closed".to_owned()); }
        let host = self.host.upgrade().ok_or_else(|| "Radius relay host is disconnected".to_owned())?;
        host.send_data(&self.connection_id, chunk).await
    }
    pub async fn close(&self, final_chunk: Option<&[u8]>) -> Result<(), String> {
        if self.closed.swap(true, std::sync::atomic::Ordering::SeqCst) { return Ok(()); }
        let Some(host) = self.host.upgrade() else { return Ok(()) };
        host.server_close_connection(&self.connection_id, final_chunk.map(<[u8]>::to_vec)).await
    }
    fn mark_closed(&self) { self.closed.store(true, std::sync::atomic::Ordering::SeqCst); }
}

/// senpi `RadiusRelayHostOptions`.
pub struct RadiusRelayHostOptions {
    pub server_id: String,
    pub server: std::sync::Arc<dyn RelayServer>,
    pub auth: std::sync::Arc<crate::experimental::radius_auth::RadiusRelayAuthResolver>,
    pub web_socket_factory: Option<RadiusRelayWebSocketFactory>,
    pub on_status: Option<std::sync::Arc<dyn Fn(RadiusRelayHostStatus) + Send + Sync>>,
}

/// senpi `RadiusRelayHost`: maintains the experimental server's multiplexed, authenticated Radius
/// host connection, reconnecting with backoff (1 s doubling to 30 s).
pub struct RadiusRelayHost {
    options: std::sync::Arc<RadiusRelayHostOptions>,
    shared: std::sync::Arc<HostShared>,
    abort: maho_ai::utils::abort::AbortController,
    socket: std::sync::Arc<std::sync::Mutex<Option<std::sync::Arc<dyn RadiusRelayWebSocket>>>>,
    run: std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>,
    closed: std::sync::Arc<std::sync::atomic::AtomicBool>,
}
impl RadiusRelayHost {
    pub fn new(options: RadiusRelayHostOptions) -> Self {
        Self {
            options: std::sync::Arc::new(options),
            shared: std::sync::Arc::new(HostShared { writer: std::sync::Mutex::new(None), connections: std::sync::Mutex::default() }),
            abort: maho_ai::utils::abort::AbortController::new(),
            socket: std::sync::Arc::default(),
            run: std::sync::Mutex::new(None),
            closed: std::sync::Arc::default(),
        }
    }
    /// senpi `start()`: idempotent.
    pub fn start(&self) {
        let mut run = lock(&self.run);
        if run.is_some() { return; }
        let host = HostRun { options: self.options.clone(), shared: self.shared.clone(), signal: self.abort.signal(), socket: self.socket.clone(), closed: self.closed.clone() };
        *run = Some(tokio::spawn(host.run()));
    }
    /// senpi `close()`.
    pub async fn close(&self) {
        if self.closed.swap(true, std::sync::atomic::Ordering::SeqCst) { return; }
        self.abort.abort(None);
        if let Some(writer) = lock(&self.shared.writer).as_ref() { writer.close(); }
        if let Some(socket) = lock(&self.socket).as_ref() { socket.close(1000, "Pi server stopped"); }
        self.shared.drop_connections(None);
        let run = lock(&self.run).take();
        if let Some(run) = run { let _ = run.await; }
    }
}

struct HostRun {
    options: std::sync::Arc<RadiusRelayHostOptions>,
    shared: std::sync::Arc<HostShared>,
    signal: maho_ai::utils::abort::AbortSignal,
    socket: std::sync::Arc<std::sync::Mutex<Option<std::sync::Arc<dyn RadiusRelayWebSocket>>>>,
    closed: std::sync::Arc<std::sync::atomic::AtomicBool>,
}
impl HostRun {
    fn closed(&self) -> bool { self.closed.load(std::sync::atomic::Ordering::SeqCst) }
    fn status(&self, status: RadiusRelayHostStatus) { if let Some(on_status) = &self.options.on_status { on_status(status); } }
    async fn run(self) {
        let mut retry_ms = HOST_RETRY_INITIAL_MS;
        while !self.closed() {
            let error = match self.connect().await {
                Ok(None) => {
                    self.status(RadiusRelayHostStatus::NotAuthenticated);
                    if !delay(MISSING_AUTH_RETRY_MS, &self.signal).await { break; }
                    continue;
                }
                Ok(Some(socket)) => {
                    retry_ms = HOST_RETRY_INITIAL_MS;
                    self.status(RadiusRelayHostStatus::Connected);
                    match self.serve(socket).await {
                        Ok(()) if self.closed() => break,
                        Ok(()) => "Radius relay host disconnected".to_owned(),
                        Err(error) => error,
                    }
                }
                Err(error) => error,
            };
            if self.closed() || self.signal.aborted() { break; }
            self.status(RadiusRelayHostStatus::Retrying { error });
            if !delay(retry_ms, &self.signal).await { break; }
            retry_ms = (retry_ms * 2).min(HOST_RETRY_MAX_MS);
        }
    }
    /// One connection attempt: `None` when no credential is configured.
    async fn connect(&self) -> Result<Option<std::sync::Arc<dyn RadiusRelayWebSocket>>, String> {
        let Some(auth) = self.options.auth.resolve(false, Some(&self.signal)).await? else { return Ok(None) };
        self.status(RadiusRelayHostStatus::Connecting);
        let factory = self.options.web_socket_factory.clone().unwrap_or_else(default_web_socket_factory);
        let url = relay_websocket_url(&auth.gateway, &self.options.server_id)?;
        open_radius_relay_websocket(url, RADIUS_RELAY_HOST_SUBPROTOCOL, &auth.token, &factory, Some(&self.signal)).await.map(Some)
    }
    /// senpi `#serve(socket)`: dispatches host messages until the socket closes; a clean (1000)
    /// close resolves, anything else is the retry error.
    async fn serve(&self, socket: std::sync::Arc<dyn RadiusRelayWebSocket>) -> Result<(), String> {
        let writer = OrderedWebSocketWriter::new(socket.clone());
        *lock(&self.socket) = Some(socket.clone());
        *lock(&self.shared.writer) = Some(writer.clone());
        let result = loop {
            let event = tokio::select! {
                () = self.signal.cancelled() => break Ok(()),
                event = socket.next_event() => event,
            };
            match event {
                None => break Err("Radius relay host closed (1006)".to_owned()),
                Some(RelaySocketEvent::Close { code: 1000, .. }) => break Ok(()),
                Some(RelaySocketEvent::Close { code, reason }) => break Err(if reason.is_empty() { format!("Radius relay host closed ({code})") } else { format!("Radius relay host closed ({code}: {reason})") }),
                Some(RelaySocketEvent::Error(message)) => break Err(web_socket_error(&message)),
                Some(RelaySocketEvent::Message(frame)) => {
                    if let Err(error) = self.handle_host_message(frame) {
                        socket.close(LOCAL_PROTOCOL_ERROR_CLOSE_CODE, "Radius relay protocol error");
                        break Err(error);
                    }
                }
            }
        };
        writer.close();
        {
            let mut current = lock(&self.socket);
            if current.as_ref().is_some_and(|current| std::sync::Arc::ptr_eq(current, &socket)) {
                *current = None;
                *lock(&self.shared.writer) = None;
            }
        }
        self.shared.drop_connections(result.as_ref().err().map(String::as_str));
        result
    }
    /// senpi `#handleHostMessage`. Writes are spawned (senpi's `void` sends).
    fn handle_host_message(&self, frame: RelayFrame) -> Result<(), String> {
        let bytes = match frame {
            RelayFrame::Text(text) => {
                match parse_host_control_message(&text)? {
                    HostControlMessage::Ping => {
                        let (shared, socket) = (self.shared.clone(), self.socket.clone());
                        tokio::spawn(async move {
                            if shared.send_control(serde_json::json!({"version": 1, "type": "pong"})).await.is_err() {
                                if let Some(socket) = lock(&socket).as_ref() { socket.close(LOCAL_TRANSPORT_ERROR_CLOSE_CODE, "Radius relay send failed"); }
                            }
                        });
                    }
                    HostControlMessage::Pong => {}
                    HostControlMessage::ConnectionOpen { connection_id } => self.open_connection(connection_id)?,
                    HostControlMessage::ConnectionClose { connection_id, .. } => self.remote_close_connection(&connection_id),
                }
                return Ok(());
            }
            RelayFrame::Binary(bytes) => bytes,
        };
        let (connection_id, payload) = parse_relay_data_frame(&bytes).ok_or_else(|| "Invalid Radius relay data frame".to_owned())?;
        let handler = lock(&self.shared.connections).get(&connection_id).map(|(_, handler)| handler.clone());
        match handler {
            Some(handler) => handler.on_data(&payload),
            None => {
                let shared = self.shared.clone();
                tokio::spawn(async move {
                    let _ = shared.send_control(serde_json::json!({"version": 1, "type": "connection_close", "connection_id": connection_id, "code": 1000})).await;
                });
            }
        }
        Ok(())
    }
    fn open_connection(&self, connection_id: String) -> Result<(), String> {
        if lock(&self.shared.connections).contains_key(&connection_id) { return Err("Radius relay reused a connection ID".to_owned()); }
        let connection = std::sync::Arc::new(RelayServerByteConnection { connection_id: connection_id.clone(), host: std::sync::Arc::downgrade(&self.shared), closed: std::sync::atomic::AtomicBool::new(false) });
        let handler = self.options.server.accept(connection.clone());
        if connection.closed() {
            let shared = self.shared.clone();
            tokio::spawn(async move {
                let _ = shared.send_control(serde_json::json!({"version": 1, "type": "connection_close", "connection_id": connection_id, "code": 1012})).await;
            });
        } else {
            lock(&self.shared.connections).insert(connection_id, (connection, handler));
        }
        Ok(())
    }
    fn remote_close_connection(&self, connection_id: &str) {
        let Some((connection, handler)) = lock(&self.shared.connections).remove(connection_id) else { return };
        connection.mark_closed();
        handler.on_close();
    }
}

// --- maho-server adapter (senpi passes the pi-server `Server`, whose `accept` this is) ---------

/// `server.accept(connection)` over maho-server's [`maho_server::server::Server::serve`]: each relay
/// client becomes one served byte connection; its chunks are forwarded in order.
pub struct ServerRelayAcceptor {
    server: std::sync::Arc<maho_server::server::Server>,
    shutdown: tokio::sync::watch::Sender<bool>,
}
impl ServerRelayAcceptor {
    pub fn new(server: std::sync::Arc<maho_server::server::Server>) -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self { server, shutdown: tokio::sync::watch::channel(false).0 })
    }
}
impl Drop for ServerRelayAcceptor {
    fn drop(&mut self) { let _ = self.shutdown.send(true); }
}
struct ServedRelayConnection(std::sync::Arc<RelayServerByteConnection>);
impl maho_server::server::types::ByteConnection for ServedRelayConnection {
    fn closed(&self) -> bool { self.0.closed() }
    fn send<'a>(&'a self, chunk: &'a [u8]) -> maho_server::server::types::ServerFuture<'a, ()> {
        Box::pin(async move { self.0.send(chunk).await.map_err(|error| maho_server::server::errors::ServerError::new("internal_error", &error)) })
    }
    fn close<'a>(&'a self, final_chunk: Option<&'a [u8]>) -> maho_server::server::types::ServerFuture<'a, ()> {
        Box::pin(async move { self.0.close(final_chunk).await.map_err(|error| maho_server::server::errors::ServerError::new("internal_error", &error)) })
    }
}
type Inbound = Result<Vec<u8>, maho_server::server::errors::ServerError>;
struct ServedRelayHandler(std::sync::Mutex<Option<tokio::sync::mpsc::UnboundedSender<Inbound>>>);
impl RelayByteConnectionHandler for ServedRelayHandler {
    fn on_data(&self, chunk: &[u8]) {
        if let Some(sender) = lock(&self.0).as_ref() { let _ = sender.send(Ok(chunk.to_vec())); }
    }
    fn on_close(&self) { lock(&self.0).take(); }
    fn on_error(&self, error: &str) {
        if let Some(sender) = lock(&self.0).take() { let _ = sender.send(Err(maho_server::server::errors::ServerError::new("internal_error", error))); }
    }
}
impl RelayServer for ServerRelayAcceptor {
    fn accept(&self, connection: std::sync::Arc<RelayServerByteConnection>) -> std::sync::Arc<dyn RelayByteConnectionHandler> {
        let (queue, mut queued) = tokio::sync::mpsc::unbounded_channel::<Inbound>();
        let (inbound, received) = tokio::sync::mpsc::channel::<Inbound>(64);
        // The relay delivers synchronously; the server reads with backpressure. The forwarder keeps
        // chunk order and ends the inbound stream when the relay side closes.
        tokio::spawn(async move {
            while let Some(chunk) = queued.recv().await {
                if inbound.send(chunk).await.is_err() { break; }
            }
        });
        let server = self.server.clone();
        let shutdown = self.shutdown.subscribe();
        tokio::spawn(async move {
            let _ = server.serve(std::sync::Arc::new(ServedRelayConnection(connection)), received, shutdown).await;
        });
        std::sync::Arc::new(ServedRelayHandler(std::sync::Mutex::new(Some(queue))))
    }
}

// --- Client transport (senpi `createRadiusClientTransportFactory`) -----------------------------

/// senpi `createRadiusClientTransportFactory({ serverId, auth, webSocketFactory })`.
pub struct RadiusClientTransportFactory {
    pub server_id: String,
    pub auth: std::sync::Arc<crate::experimental::radius_auth::RadiusRelayAuthResolver>,
    pub web_socket_factory: Option<RadiusRelayWebSocketFactory>,
}
pub fn create_radius_client_transport_factory(server_id: &str, auth: std::sync::Arc<crate::experimental::radius_auth::RadiusRelayAuthResolver>, web_socket_factory: Option<RadiusRelayWebSocketFactory>) -> RadiusClientTransportFactory {
    RadiusClientTransportFactory { server_id: server_id.to_owned(), auth, web_socket_factory }
}
impl maho_server::client::transport::ByteTransportFactory for RadiusClientTransportFactory {
    fn connect(&self, handlers: maho_server::client::transport::ByteTransportHandlers) -> maho_server::client::transport::TransportFuture<'_, std::sync::Arc<dyn maho_server::client::transport::ByteTransport>> {
        use maho_server::client::errors::ClientError;
        Box::pin(async move {
            let auth = self.auth.resolve(true, None).await.map_err(ClientError::Disconnected)?.ok_or_else(|| ClientError::Disconnected("Radius authentication is required".to_owned()))?;
            let factory = self.web_socket_factory.clone().unwrap_or_else(default_web_socket_factory);
            let url = relay_websocket_url(&auth.gateway, &self.server_id).map_err(ClientError::Disconnected)?;
            let socket = open_radius_relay_websocket(url, RADIUS_RELAY_CLIENT_SUBPROTOCOL, &auth.token, &factory, None).await.map_err(ClientError::Disconnected)?;
            Ok(RadiusClientByteTransport::start(socket, handlers) as std::sync::Arc<dyn maho_server::client::transport::ByteTransport>)
        })
    }
}

/// senpi `RadiusClientByteTransport`: the raw relay WebSocket as the client's byte transport.
struct RadiusClientByteTransport {
    socket: std::sync::Arc<dyn RadiusRelayWebSocket>,
    writer: std::sync::Arc<OrderedWebSocketWriter>,
    closed: std::sync::atomic::AtomicBool,
}
impl RadiusClientByteTransport {
    fn start(socket: std::sync::Arc<dyn RadiusRelayWebSocket>, handlers: maho_server::client::transport::ByteTransportHandlers) -> std::sync::Arc<Self> {
        let transport = std::sync::Arc::new(Self { writer: OrderedWebSocketWriter::new(socket.clone()), socket, closed: std::sync::atomic::AtomicBool::new(false) });
        let weak = std::sync::Arc::downgrade(&transport);
        let socket = transport.socket.clone();
        tokio::spawn(async move {
            use maho_server::client::errors::ClientError;
            while let Some(event) = socket.next_event().await {
                let Some(transport) = weak.upgrade() else { return };
                if transport.is_closed() { return; }
                match event {
                    RelaySocketEvent::Message(RelayFrame::Binary(bytes)) => (handlers.on_data)(&bytes),
                    RelaySocketEvent::Message(RelayFrame::Text(_)) => {
                        if transport.fail() { (handlers.on_error)(ClientError::Protocol("Radius relay client received a non-binary message".to_owned())); }
                        return;
                    }
                    RelaySocketEvent::Close { .. } => {
                        if transport.mark_closed() { (handlers.on_close)(); }
                        return;
                    }
                    RelaySocketEvent::Error(message) => {
                        if transport.fail() { (handlers.on_error)(ClientError::Disconnected(web_socket_error(&message))); }
                        return;
                    }
                }
            }
            if weak.upgrade().is_some_and(|transport| transport.mark_closed()) { (handlers.on_close)(); }
        });
        transport
    }
    fn is_closed(&self) -> bool { self.closed.load(std::sync::atomic::Ordering::SeqCst) }
    fn mark_closed(&self) -> bool {
        if self.closed.swap(true, std::sync::atomic::Ordering::SeqCst) { return false; }
        self.writer.close();
        true
    }
    /// senpi `#fail`: true when this call closed the transport (the caller reports the error).
    fn fail(&self) -> bool {
        if !self.mark_closed() { return false; }
        self.socket.close(LOCAL_TRANSPORT_ERROR_CLOSE_CODE, "Radius relay transport error");
        true
    }
}
impl maho_server::client::transport::ByteTransport for RadiusClientByteTransport {
    fn send<'a>(&'a self, chunk: &'a [u8]) -> maho_server::client::transport::TransportFuture<'a, ()> {
        Box::pin(async move {
            if self.is_closed() { return Err(maho_server::client::errors::ClientError::Disconnected("Radius relay client is closed".to_owned())); }
            self.writer.send(RelayFrame::Binary(chunk.to_vec())).await.map_err(maho_server::client::errors::ClientError::Disconnected)
        })
    }
    fn close(&self) {
        if self.mark_closed() { self.socket.close(1000, "Pi client closed"); }
    }
}

// --- Default WebSocket (senpi `defaultWebSocketFactory`, undici) -------------------------------

type TungsteniteStream = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;
struct TungsteniteRelaySocket {
    protocol: String,
    sink: std::sync::Arc<tokio::sync::Mutex<futures::stream::SplitSink<TungsteniteStream, tokio_tungstenite::tungstenite::Message>>>,
    stream: tokio::sync::Mutex<futures::stream::SplitStream<TungsteniteStream>>,
}
#[async_trait::async_trait]
impl RadiusRelayWebSocket for TungsteniteRelaySocket {
    fn protocol(&self) -> String { self.protocol.clone() }
    async fn send(&self, frame: RelayFrame) -> Result<(), String> {
        use futures::SinkExt;
        use tokio_tungstenite::tungstenite::Message;
        let message = match frame { RelayFrame::Text(text) => Message::Text(text.into()), RelayFrame::Binary(bytes) => Message::Binary(bytes.into()) };
        self.sink.lock().await.send(message).await.map_err(|error| error.to_string())
    }
    fn close(&self, code: u16, reason: &str) {
        use futures::SinkExt;
        use tokio_tungstenite::tungstenite::{Message, protocol::CloseFrame};
        let (sink, frame) = (self.sink.clone(), CloseFrame { code: code.into(), reason: reason.to_owned().into() });
        tokio::spawn(async move { let _ = sink.lock().await.send(Message::Close(Some(frame))).await; });
    }
    async fn next_event(&self) -> Option<RelaySocketEvent> {
        use futures::StreamExt;
        use tokio_tungstenite::tungstenite::Message;
        let mut stream = self.stream.lock().await;
        loop {
            return Some(match stream.next().await? {
                Ok(Message::Text(text)) => RelaySocketEvent::Message(RelayFrame::Text(text.as_str().to_owned())),
                Ok(Message::Binary(bytes)) => RelaySocketEvent::Message(RelayFrame::Binary(bytes.to_vec())),
                Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_)) => continue,
                Ok(Message::Close(frame)) => RelaySocketEvent::Close { code: frame.as_ref().map_or(1005, |frame| u16::from(frame.code)), reason: frame.map(|frame| frame.reason.as_str().to_owned()).unwrap_or_default() },
                Err(error) => RelaySocketEvent::Error(error.to_string()),
            });
        }
    }
}
fn default_web_socket_factory() -> RadiusRelayWebSocketFactory {
    std::sync::Arc::new(|request: RelaySocketRequest| -> futures::future::BoxFuture<'static, Result<std::sync::Arc<dyn RadiusRelayWebSocket>, String>> {
        Box::pin(async move {
            use futures::StreamExt;
            use tokio_tungstenite::tungstenite::{client::IntoClientRequest, http::HeaderValue};
            let mut client = request.url.as_str().into_client_request().map_err(|error| error.to_string())?;
            let headers = client.headers_mut();
            headers.insert("sec-websocket-protocol", HeaderValue::from_str(&request.protocol).map_err(|error| error.to_string())?);
            headers.insert("authorization", HeaderValue::from_str(&request.authorization).map_err(|error| error.to_string())?);
            let (socket, response) = tokio_tungstenite::connect_async(client).await.map_err(|error| error.to_string())?;
            let protocol = response.headers().get("sec-websocket-protocol").and_then(|value| value.to_str().ok()).unwrap_or_default().to_owned();
            let (sink, stream) = socket.split();
            Ok(std::sync::Arc::new(TungsteniteRelaySocket { protocol, sink: std::sync::Arc::new(tokio::sync::Mutex::new(sink)), stream: tokio::sync::Mutex::new(stream) }) as std::sync::Arc<dyn RadiusRelayWebSocket>)
        })
    })
}
