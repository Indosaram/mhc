use crate::host_protocol_info::{HostProtocolInfo, parse_host_protocol_info};
use serde_json::Value;
#[cfg(unix)]
use tokio::io::{AsyncReadExt, AsyncWriteExt};
pub const DEFAULT_PROBE_TIMEOUT_MS: u64 = 10000;
pub const DETACH_CONNECTION_CAPABILITY: &str = "detach_connection";
pub struct ProbeHostOptions<'a> {
    pub socket: &'a str,
    pub timeout_ms: Option<u64>,
}
/// The running host's identity, or `None` when nothing is serving the endpoint.
pub async fn probe_host(options: ProbeHostOptions<'_>) -> Option<HostProtocolInfo> {
    probe_protocol_info(
        options.socket,
        options.timeout_ms.unwrap_or(DEFAULT_PROBE_TIMEOUT_MS),
    )
    .await
}
const PROBE_REQUEST_ID: &str = "ensure-host-probe";
#[derive(Default)]
pub struct ProbeOutcome {
    pub connected: bool,
    pub answer: Option<Value>,
}
pub fn read_answer(text: &str) -> Option<Value> {
    let parsed: Value = serde_json::from_str(text).ok()?;
    if parsed["id"] != PROBE_REQUEST_ID || parsed["success"] != true {
        return None;
    }
    Some(
        parsed
            .get("data")
            .filter(|value| !value.is_null())
            .cloned()
            .unwrap_or_else(|| serde_json::json!({})),
    )
}
pub async fn connect_and_ask(socket_path: &str, request: &Value, timeout_ms: u64) -> ProbeOutcome {
    connect_and_ask_with_secret(socket_path, None, request, timeout_ms).await
}
/// `connect_and_ask` with an explicit transport secret: a supervisor's internal named pipe has no `.secret` sibling to
/// read, so its secret is passed in. `None` reads the secret file of a logical path (win32); unix ignores it.
pub async fn connect_and_ask_with_secret(
    socket_path: &str,
    secret: Option<&[u8]>,
    request: &Value,
    timeout_ms: u64,
) -> ProbeOutcome {
    #[cfg(unix)]
    {
        let _ = secret;
        let mut outcome = ProbeOutcome::default();
        let _ = tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), async {
            let mut socket = tokio::net::UnixStream::connect(socket_path).await.ok()?;
            outcome.connected = true;
            socket
                .write_all(crate::jsonl::serialize_json_line(request).ok()?.as_bytes())
                .await
                .ok()?;
            let mut buffer = String::new();
            let mut chunk = [0u8; 8192];
            loop {
                let size = socket.read(&mut chunk).await.ok()?;
                if size == 0 {
                    return None;
                }
                buffer.push_str(&String::from_utf8_lossy(&chunk[..size]));
                while let Some(newline) = buffer.find('\n') {
                    let line = buffer[..newline].to_owned();
                    buffer.drain(..=newline);
                    if let Some(answer) = read_answer(&line) {
                        outcome.answer = Some(answer);
                        return Some(());
                    }
                }
            }
        })
        .await;
        outcome
    }
    #[cfg(windows)]
    {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let secret: Vec<u8> = match secret {
            Some(secret) => secret.to_vec(),
            None => {
                let secret_path = match crate::socket_transport::socket_secret_path(socket_path) {
                    Ok(path) => path,
                    Err(_) => return ProbeOutcome::default(),
                };
                match crate::socket_transport::read_socket_secret(std::path::Path::new(
                    &secret_path,
                )) {
                    Ok(secret) => secret.to_vec(),
                    Err(_) => return ProbeOutcome::default(),
                }
            }
        };
        let address = match crate::socket_transport::resolve_socket_transport_address(
            socket_path,
            "win32",
            Some(&secret),
        ) {
            Ok(address) => address,
            Err(_) => return ProbeOutcome::default(),
        };
        let mut outcome = ProbeOutcome::default();
        let _ = tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), async {
            let mut stream = crate::host_lifecycle::win32_pipe::connect(
                &address,
                std::time::Duration::from_millis(timeout_ms),
            )
            .await
            .ok()?;
            outcome.connected = true;
            crate::socket_transport::send_socket_handshake(&mut stream, &secret)
                .await
                .ok()?;
            stream
                .write_all(crate::jsonl::serialize_json_line(request).ok()?.as_bytes())
                .await
                .ok()?;
            let mut buffer = String::new();
            let mut chunk = [0u8; 8192];
            loop {
                let size = stream.read(&mut chunk).await.ok()?;
                if size == 0 {
                    return None;
                }
                buffer.push_str(&String::from_utf8_lossy(&chunk[..size]));
                while let Some(newline) = buffer.find('\n') {
                    let line = buffer[..newline].to_owned();
                    buffer.drain(..=newline);
                    if let Some(answer) = read_answer(&line) {
                        outcome.answer = Some(answer);
                        return Some(());
                    }
                }
            }
        })
        .await;
        outcome
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (socket_path, secret, request, timeout_ms);
        ProbeOutcome::default()
    }
}
pub async fn request_on_socket(
    socket_path: &str,
    command: &Value,
    timeout_ms: u64,
) -> Option<Value> {
    let mut request = serde_json::json!({"id":PROBE_REQUEST_ID});
    if let Some(fields) = command.as_object() {
        request.as_object_mut()?.extend(fields.clone());
    }
    connect_and_ask(socket_path, &request, timeout_ms)
        .await
        .answer
}
pub async fn probe_protocol_info(socket_path: &str, timeout_ms: u64) -> Option<HostProtocolInfo> {
    probe_protocol_info_with_secret(socket_path, None, timeout_ms).await
}
pub async fn probe_protocol_info_with_secret(
    socket_path: &str,
    secret: Option<&[u8]>,
    timeout_ms: u64,
) -> Option<HostProtocolInfo> {
    let request = serde_json::json!({"id":PROBE_REQUEST_ID,"type":"get_protocol_info"});
    parse_host_protocol_info(
        &connect_and_ask_with_secret(socket_path, secret, &request, timeout_ms)
            .await
            .answer?,
    )
}
pub async fn probe_socket_reachable(socket_path: &str, timeout_ms: u64) -> bool {
    connect_and_ask(
        socket_path,
        &serde_json::json!({"id":PROBE_REQUEST_ID,"type":"get_protocol_info"}),
        timeout_ms,
    )
    .await
    .connected
}
pub async fn probe_session_count(socket_path: &str, timeout_ms: u64) -> Option<usize> {
    request_on_socket(
        socket_path,
        &serde_json::json!({"type":"list_sessions","include_workers":true}),
        timeout_ms,
    )
    .await?
    .get("sessions")?
    .as_array()
    .map(Vec::len)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reply_matches_id_and_success() {
        assert!(read_answer(r#"{"id":"other","success":true}"#).is_none());
        assert!(read_answer(r#"{"id":"ensure-host-probe","success":false}"#).is_none());
        assert_eq!(
            read_answer(r#"{"id":"ensure-host-probe","success":true,"data":null}"#),
            Some(serde_json::json!({}))
        );
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn actual_socket_ignores_broadcast_and_unrelated_reply() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("probe.sock");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        let serve = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut byte = [0u8; 1];
            let mut request = vec![];
            loop {
                socket.read_exact(&mut byte).await.unwrap();
                request.push(byte[0]);
                if byte[0] == b'\n' {
                    break;
                }
            }
            let request: Value = serde_json::from_slice(&request).unwrap();
            assert_eq!(request["id"], PROBE_REQUEST_ID);
            socket.write_all(b"{\"type\":\"agent_start\"}\n{\"id\":\"other\",\"success\":true}\n{\"id\":\"ensure-host-probe\",\"success\":true,\"data\":{\"sessions\":[{},{}]}}\n").await.unwrap();
        });
        assert_eq!(
            probe_session_count(path.to_str().unwrap(), 1000).await,
            Some(2)
        );
        serve.await.unwrap();
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn absent_endpoint_is_not_reachable() {
        let temp = tempfile::tempdir().unwrap();
        assert!(!probe_socket_reachable(temp.path().join("missing").to_str().unwrap(), 1000).await);
    }
}
