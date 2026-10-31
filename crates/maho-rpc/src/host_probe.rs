use crate::host_protocol_info::{HostProtocolInfo, parse_host_protocol_info};
use serde_json::Value;
pub const DEFAULT_PROBE_TIMEOUT_MS: u64 = 10000;
pub const DETACH_CONNECTION_CAPABILITY: &str = "detach_connection";
pub struct ProbeHostOptions<'a> {
    pub socket: &'a str,
    pub timeout_ms: Option<u64>,
    /// Credential lookup only (senpi `RpcKeyOptions.keyRoot`); never selects a configuration profile.
    pub key_root: Option<&'a std::path::Path>,
}
/// The running host's identity, or `None` when nothing is serving the endpoint.
pub async fn probe_host(options: ProbeHostOptions<'_>) -> Option<HostProtocolInfo> {
    let probe = probe_protocol_info(
        options.socket,
        options.timeout_ms.unwrap_or(DEFAULT_PROBE_TIMEOUT_MS),
    );
    match options.key_root {
        Some(root) => with_rpc_key_root(root.to_path_buf(), probe).await,
        None => probe.await,
    }
}

tokio::task_local! {
    static RPC_KEY_ROOT: std::path::PathBuf;
}
/// Runs `operation` with `root` as the key root every endpoint proof inside it reads (senpi rpc-auth-client
/// `withRpcKeyRoot`); concurrent operations keep separate roots.
pub async fn with_rpc_key_root<F: std::future::Future>(
    root: std::path::PathBuf,
    operation: F,
) -> F::Output {
    RPC_KEY_ROOT.scope(root, operation).await
}
/// senpi rpc-auth-client `rpcKeyRoot`: the explicit option, then an enclosing [`with_rpc_key_root`], then
/// `SENPI_RPC_KEY_ROOT`, then the caller's default (a target's agent dir), then this profile's agent dir.
pub fn rpc_key_root(
    explicit: Option<&std::path::Path>,
    default_root: Option<&std::path::Path>,
) -> std::path::PathBuf {
    rpc_key_root_with(
        explicit,
        std::env::var("SENPI_RPC_KEY_ROOT").ok().as_deref(),
        default_root,
    )
}
/// [`rpc_key_root`] with the environment value passed in.
pub fn rpc_key_root_with(
    explicit: Option<&std::path::Path>,
    env_root: Option<&str>,
    default_root: Option<&std::path::Path>,
) -> std::path::PathBuf {
    if let Some(explicit) = explicit {
        return explicit.to_path_buf();
    }
    if let Ok(scoped) = RPC_KEY_ROOT.try_with(Clone::clone) {
        return scoped;
    }
    env_root
        .filter(|root| !root.trim().is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| default_root.map(ToOwned::to_owned))
        .unwrap_or_else(default_key_root)
}
/// This profile's agent dir, read the way a client resolves it without a settings manager.
fn default_key_root() -> std::path::PathBuf {
    for name in [
        "OMO_CODING_AGENT_DIR",
        "SENPI_CODING_AGENT_DIR",
        "PI_CODING_AGENT_DIR",
    ] {
        if let Ok(value) = std::env::var(name)
            && !value.trim().is_empty()
        {
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

/// `probe_protocol_info` for a read that must not count as host activity, such as `host status`.
pub async fn observe_protocol_info(socket_path: &str, timeout_ms: u64) -> Option<HostProtocolInfo> {
    let request = serde_json::json!({
        "id": PROBE_REQUEST_ID,
        "type": "get_protocol_info",
        crate::host_observe_request::OBSERVE_REQUEST_FIELD: true,
    });
    parse_host_protocol_info(
        &connect_and_ask(socket_path, &request, timeout_ms)
            .await
            .answer?,
    )
}

/// The session count together with the connection that answered it, kept open so the SAME host can be asked
/// again after its public path was handed to somebody else: the recount reaches the process that was counted,
/// never whoever serves the path by then (senpi `HeldSessionCount`).
#[cfg(unix)]
pub struct HeldSessionCount {
    pub sessions: usize,
    reader: tokio::io::BufReader<tokio::net::UnixStream>,
    asked: usize,
}
#[cfg(unix)]
impl HeldSessionCount {
    /// Asks the counted host again over the held connection; `None` when it no longer answers.
    pub async fn recount(&mut self, timeout_ms: u64) -> Option<usize> {
        self.asked += 1;
        let id = format!("{PROBE_REQUEST_ID}-recount-{}", self.asked);
        let request =
            serde_json::json!({"id": id, "type": "list_sessions", "include_workers": true});
        let answer = ask_on(&mut self.reader, &request, timeout_ms).await?;
        session_count_of(&answer)
    }
}
#[cfg(unix)]
async fn ask_on(
    reader: &mut tokio::io::BufReader<tokio::net::UnixStream>,
    request: &Value,
    timeout_ms: u64,
) -> Option<Value> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
    let id = request["id"].clone();
    tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), async {
        reader
            .get_mut()
            .write_all(format!("{request}\n").as_bytes())
            .await
            .ok()?;
        loop {
            let mut line = Vec::new();
            if reader.read_until(b'\n', &mut line).await.ok()? == 0 {
                return None;
            }
            let Ok(frame) = serde_json::from_slice::<Value>(&line) else {
                continue;
            };
            if frame["id"] == id && frame["success"] == true {
                return Some(frame.get("data").cloned().unwrap_or(Value::Null));
            }
        }
    })
    .await
    .ok()
    .flatten()
}
#[cfg(unix)]
fn session_count_of(reply: &Value) -> Option<usize> {
    reply.get("sessions")?.as_array().map(Vec::len)
}
/// senpi `holdSessionCount`: `None` when the host does not answer with a session list.
#[cfg(unix)]
pub async fn hold_session_count(socket_path: &str, timeout_ms: u64) -> Option<HeldSessionCount> {
    let request = serde_json::json!({"id": PROBE_REQUEST_ID, "type": "list_sessions", "include_workers": true});
    let (socket, _) = authenticated_host_socket(socket_path, &request, timeout_ms, None)
        .await
        .ok()?;
    let mut reader = tokio::io::BufReader::new(socket.into_stream().ok()?);
    let answer = ask_on(&mut reader, &request, timeout_ms).await?;
    Some(HeldSessionCount {
        sessions: session_count_of(&answer)?,
        reader,
        asked: 0,
    })
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
        // senpi rpc-auth-client.ts `connectAuthenticatedRpcSocket` default first frame.
        let mut first = serde_json::json!({"type":"get_protocol_info","observe":true});
        if let Some(origin) = crate::host_observe_request::process_origin_host_instance() {
            first[crate::host_observe_request::ORIGIN_HOST_INSTANCE_FIELD] = origin.into();
        }
        first
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
        let root = rpc_key_root(None, agent_dir);
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
    #[tokio::test]
    async fn key_root_prefers_explicit_then_scope_then_env_then_default() {
        use std::path::Path;
        assert_eq!(
            rpc_key_root_with(
                Some(Path::new("/flag")),
                Some("/env"),
                Some(Path::new("/d"))
            ),
            Path::new("/flag")
        );
        assert_eq!(
            rpc_key_root_with(None, Some("/env"), Some(Path::new("/d"))),
            Path::new("/env")
        );
        assert_eq!(
            rpc_key_root_with(None, Some(" "), Some(Path::new("/d"))),
            Path::new("/d")
        );
        let scoped = with_rpc_key_root("/scope".into(), async {
            tokio::task::yield_now().await;
            (
                rpc_key_root_with(None, Some("/env"), None),
                rpc_key_root_with(Some(Path::new("/client")), Some("/env"), None),
            )
        })
        .await;
        assert_eq!(scoped.0, Path::new("/scope"));
        assert_eq!(scoped.1, Path::new("/client"));
        // Separate roots across concurrent operations.
        let (a, b) = tokio::join!(
            with_rpc_key_root("/root-a".into(), async {
                tokio::task::yield_now().await;
                rpc_key_root_with(None, None, None)
            }),
            with_rpc_key_root("/root-b".into(), async {
                tokio::task::yield_now().await;
                rpc_key_root_with(None, None, None)
            })
        );
        assert_eq!(
            (a.as_path(), b.as_path()),
            (Path::new("/root-a"), Path::new("/root-b"))
        );
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn a_held_session_count_recounts_over_the_same_connection() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("count.sock");
        let listener = tokio::net::UnixListener::bind(&path).unwrap();
        let serve = tokio::spawn(async move {
            use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
            let (socket, _) = listener.accept().await.unwrap();
            let mut socket = tokio::io::BufReader::new(socket);
            let mut sessions = 2;
            loop {
                let mut line = String::new();
                if socket.read_line(&mut line).await.unwrap() == 0 {
                    return;
                }
                let request: Value = serde_json::from_str(&line).unwrap();
                let data = if request["type"] == "get_protocol_info" {
                    serde_json::json!({})
                } else {
                    let listed =
                        serde_json::json!({"sessions": vec![serde_json::json!({}); sessions]});
                    sessions = 0;
                    listed
                };
                let response = serde_json::json!({"id":request["id"],"success":true,"data":data});
                socket
                    .get_mut()
                    .write_all(format!("{response}\n").as_bytes())
                    .await
                    .unwrap();
            }
        });
        let mut held = hold_session_count(path.to_str().unwrap(), 1000)
            .await
            .expect("held");
        assert_eq!(held.sessions, 2);
        // The path is gone; the held connection still reaches the counted host.
        std::fs::remove_file(&path).unwrap();
        assert_eq!(held.recount(1000).await, Some(0));
        drop(held);
        serve.await.unwrap();
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn absent_endpoint_is_not_reachable() {
        let temp = tempfile::tempdir().unwrap();
        assert!(!probe_socket_reachable(temp.path().join("missing").to_str().unwrap(), 1000).await);
    }
}
