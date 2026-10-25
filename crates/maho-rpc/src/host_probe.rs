use crate::host_protocol_info::{HostProtocolInfo, parse_host_protocol_info};
use serde_json::Value;
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
        match ask_host_authenticated(socket_path, request, timeout_ms, None).await {
            Ok(Some(answer)) => ProbeOutcome {
                connected: true,
                answer: if answer["success"] == true {
                    Some(
                        answer
                            .get("data")
                            .cloned()
                            .unwrap_or_else(|| serde_json::json!({})),
                    )
                } else {
                    None
                },
            },
            Ok(None) => ProbeOutcome::default(),
            Err(error) => {
                eprintln!("host identity verification failed: {error}");
                ProbeOutcome::default()
            }
        }
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
    #[cfg(unix)]
    {
        return tokio::time::timeout(
            std::time::Duration::from_millis(timeout_ms),
            tokio::net::UnixStream::connect(socket_path),
        )
        .await
        .is_ok_and(|result| result.is_ok());
    }
    #[cfg(not(unix))]
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

/// Authenticated raw request. Identity failures are typed, never mistaken for a missing endpoint.
#[cfg(unix)]
pub async fn ask_host_authenticated(
    endpoint: &str,
    request: &Value,
    timeout_ms: u64,
    agent_dir: Option<&std::path::Path>,
) -> Result<Option<Value>, crate::socket_auth::RpcIdentityError> {
    use crate::socket_auth;
    let (mut socket, info) =
        match authenticated_host_socket(endpoint, request, timeout_ms, agent_dir).await {
            Ok(connection) => connection,
            Err(error) => {
                return match error.downcast::<crate::socket_auth::RpcIdentityError>() {
                    Ok(error) => Err(*error),
                    Err(_) => Ok(None),
                };
            }
        };
    if request["type"] == "get_protocol_info" {
        return Ok(Some(info));
    }
    let answer = match socket.request(request.clone()).await {
        Ok(answer) => answer,
        Err(_) => return Ok(None),
    };
    if answer["success"] == false
        && (answer["errorCode"] == "auth_required" || answer["error"] == "auth_required")
    {
        return Err(socket_auth::auth_required_error(endpoint));
    }
    Ok(Some(answer))
}

#[cfg(unix)]
pub async fn authenticated_host_socket(
    endpoint: &str,
    request: &Value,
    timeout_ms: u64,
    agent_dir: Option<&std::path::Path>,
) -> Result<(crate::socket_client::RpcSocket, Value), Box<dyn std::error::Error + Send + Sync>> {
    use crate::{socket_auth, socket_client::RpcSocket};
    let mut socket =
        match RpcSocket::connect(endpoint, std::time::Duration::from_millis(timeout_ms)).await {
            Ok(socket) => socket,
            Err(error) => return Err(error.into()),
        };
    let mut random = [0u8; 32];
    getrandom::fill(&mut random).map_err(|error| std::io::Error::other(error.to_string()))?;
    let nonce = hex::encode(random);
    let mut command = if request["type"] == "get_protocol_info" {
        request.clone()
    } else {
        serde_json::json!({"type":"get_protocol_info","observe":true})
    };
    command["auth"] = serde_json::json!({"client_nonce":nonce});
    let info = match socket.request(command).await {
        Ok(info) => info,
        Err(error) => return Err(error.into()),
    };
    if let Some(auth) = info.get("data").and_then(|data| data.get("auth")) {
        let launch = auth["launch_id"]
            .as_str()
            .filter(|launch| {
                uuid::Uuid::parse_str(launch)
                    .is_ok_and(|id| id.get_version_num() == 4 && id.to_string() == *launch)
            })
            .ok_or_else(|| socket_auth::RpcIdentityError {
                code: "host_identity_unproven",
                location: endpoint.into(),
                detail: "invalid launch".into(),
            })?;
        for field in ["server_nonce", "server_proof"] {
            if !auth[field].as_str().is_some_and(|value| {
                value.len() == 64
                    && value
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            }) {
                return Err(Box::new(socket_auth::RpcIdentityError {
                    code: "host_identity_unproven",
                    location: endpoint.into(),
                    detail: format!("invalid {field}"),
                }));
            }
        }
        let root = std::env::var("SENPI_RPC_KEY_ROOT")
            .ok()
            .filter(|root| !root.trim().is_empty())
            .map(std::path::PathBuf::from)
            .or_else(|| agent_dir.map(ToOwned::to_owned))
            .unwrap_or_else(|| {
                for name in [
                    "OMO_CODING_AGENT_DIR",
                    "SENPI_CODING_AGENT_DIR",
                    "PI_CODING_AGENT_DIR",
                ] {
                    if let Ok(value) = std::env::var(name)
                        && !value.trim().is_empty() {
                            return std::path::absolute(value.trim())
                                .unwrap_or_else(|_| std::path::PathBuf::from(value.trim()));
                        }
                }
                let home = std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default());
                for path in [home.join(".omo/agent"), home.join(".omo")] {
                    if path.join("settings.json").exists() {
                        return path;
                    }
                }
                home.join(".senpi/agent")
            });
        if let Some(key) = socket_auth::read_key(&root, launch)? {
            socket_auth::verify_server(&key, auth, &nonce, endpoint)?;
            let proof = socket_auth::proof(
                &key,
                "senpi-rpc-client",
                launch,
                auth["server_nonce"].as_str().unwrap_or_default(),
                &nonce,
            );
            let answer = match socket
                .request(
                    serde_json::json!({"type":"authenticate","client_proof":hex::encode(proof)}),
                )
                .await
            {
                Ok(answer) => answer,
                Err(error) => return Err(error.into()),
            };
            if answer["success"] != true {
                return Err(Box::new(socket_auth::RpcIdentityError {
                    code: "auth_failed",
                    location: endpoint.into(),
                    detail: "client proof refused".into(),
                }));
            }
        } else {
            static GRACE_ROOTS: std::sync::OnceLock<
                std::sync::Mutex<std::collections::BTreeSet<std::path::PathBuf>>,
            > = std::sync::OnceLock::new();
            if GRACE_ROOTS
                .get_or_init(Default::default)
                .lock()
                .expect("authentication grace roots lock poisoned")
                .insert(root.clone())
            {
                eprintln!(
                    "senpi rpc client: no key record under {}; set SENPI_RPC_KEY_ROOT to the host's agent dir",
                    root.display()
                );
            }
        }
    }
    Ok((socket, info))
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
            let (socket, _) = listener.accept().await.unwrap();
            let mut socket = tokio::io::BufReader::new(socket);
            use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
            let mut line = String::new();
            socket.read_line(&mut line).await.unwrap();
            let request: Value = serde_json::from_str(&line).unwrap();
            let response = serde_json::json!({"id":request["id"],"success":true,"data":{}});
            socket
                .get_mut()
                .write_all(format!("{response}\n").as_bytes())
                .await
                .unwrap();
            line.clear();
            socket.read_line(&mut line).await.unwrap();
            let request: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(request["id"], PROBE_REQUEST_ID);
            socket.get_mut().write_all(b"{\"type\":\"agent_start\"}\n{\"id\":\"other\",\"success\":true}\n{\"id\":\"ensure-host-probe\",\"success\":true,\"data\":{\"sessions\":[{},{}]}}\n").await.unwrap();
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
