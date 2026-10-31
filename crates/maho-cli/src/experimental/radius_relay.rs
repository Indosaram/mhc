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
