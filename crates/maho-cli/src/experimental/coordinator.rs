use serde::{Deserialize, Serialize};
#[cfg(unix)]
#[path = "coordinator_runtime.rs"]
mod runtime;
#[cfg(unix)]
pub use runtime::{run_coordinator, run_coordinator_ready};
#[cfg(unix)]
pub struct CoordinatorConnection {
    pub server_connection_id: String,
    pub peer_ids: std::collections::HashSet<String>,
    connection: crate::experimental::mini::shared::transport::SocketConnection,
    replaced: bool,
}
#[cfg(unix)]
impl CoordinatorConnection {
    pub async fn connect(
        control_path: &std::path::Path,
        endpoint: &str,
        server_connection_id: String,
    ) -> Result<Self, String> {
        let socket = tokio::net::UnixStream::connect(control_path)
            .await
            .map_err(|error| error.to_string())?;
        let (input, output) = socket.into_split();
        let mut connection =
            crate::experimental::mini::shared::transport::JsonConnection::new(input, output);
        connection.send(&serde_json::json!({"type":"register_server", "protocol":COORDINATOR_PROTOCOL_VERSION, "serverConnectionId":server_connection_id, "endpoint":endpoint})).await.map_err(|error| error.to_string())?;
        let message = connection
            .receive()
            .await
            .map_err(|error| error.to_string())?
            .ok_or("Coordinator connection closed")?;
        match serde_json::from_value(message).map_err(|_| "Coordinator sent an invalid message")? {
            CoordinatorMessage::ServerRegistered {
                server_connection_id: returned,
                peers,
            } if returned == server_connection_id => Ok(Self {
                server_connection_id,
                peer_ids: peers.into_iter().collect(),
                connection,
                replaced: false,
            }),
            _ => Err("Coordinator returned an invalid server registration".to_owned()),
        }
    }
    pub fn was_replaced(&self) -> bool {
        self.replaced
    }
    pub async fn next_event(&mut self) -> Result<Option<CoordinatorMessage>, String> {
        let Some(value) = self
            .connection
            .receive()
            .await
            .map_err(|error| error.to_string())?
        else {
            self.replaced = true;
            return Ok(None);
        };
        // senpi destroys the socket on an invalid or unsupported message; the disconnect marks the connection replaced.
        let Ok(message) = serde_json::from_value::<CoordinatorMessage>(value) else {
            self.replaced = true;
            let _ = self.connection.close().await;
            return Err("Coordinator sent an invalid message".to_owned());
        };
        match &message {
            CoordinatorMessage::PeerConnected { peer_id } => {
                self.peer_ids.insert(peer_id.clone());
            }
            CoordinatorMessage::PeerDisconnected { peer_id } => {
                self.peer_ids.remove(peer_id);
            }
            CoordinatorMessage::ServerReplaced => {
                self.replaced = true;
            }
            CoordinatorMessage::Message { .. } => {}
            CoordinatorMessage::ServerRegistered { .. } => {
                self.replaced = true;
                let _ = self.connection.close().await;
                return Err("Coordinator sent an unsupported message".to_owned());
            }
        }
        Ok(Some(message))
    }
    /// senpi `onEvent` listeners plus the `replaced` promise, pulled: the next peer/message event, or `None` once the
    /// connection was replaced, closed, or broken (when `was_replaced()` is true, upstream's `replaced` has resolved).
    pub async fn next_connection_event(&mut self) -> Option<CoordinatorConnectionEvent> {
        loop {
            match self.next_event().await {
                Ok(Some(CoordinatorMessage::PeerConnected { peer_id })) => {
                    return Some(CoordinatorConnectionEvent::PeerConnected { peer_id });
                }
                Ok(Some(CoordinatorMessage::PeerDisconnected { peer_id })) => {
                    return Some(CoordinatorConnectionEvent::PeerDisconnected { peer_id });
                }
                Ok(Some(CoordinatorMessage::Message { from, payload })) => {
                    return Some(CoordinatorConnectionEvent::Message { from, payload });
                }
                Ok(Some(CoordinatorMessage::ServerRegistered { .. })) => continue,
                Ok(Some(CoordinatorMessage::ServerReplaced) | None) | Err(_) => {
                    self.replaced = true;
                    return None;
                }
            }
        }
    }
    pub async fn send(&mut self, peer_id: &str, payload: serde_json::Value) -> Result<(), String> {
        self.write(serde_json::json!({"type":"send", "to":peer_id, "payload":payload}))
            .await
    }
    pub async fn broadcast(&mut self, payload: serde_json::Value) -> Result<(), String> {
        self.write(serde_json::json!({"type":"broadcast", "payload":payload}))
            .await
    }
    async fn write(&mut self, message: serde_json::Value) -> Result<(), String> {
        if self.connection.closed() {
            return Err("Coordinator server is not connected".to_owned());
        }
        self.connection
            .send(&message)
            .await
            .map_err(|error| error.to_string())
    }
    pub async fn close(&mut self) -> Result<(), String> {
        self.peer_ids.clear();
        self.connection
            .close()
            .await
            .map_err(|error| error.to_string())
    }
}
pub const COORDINATOR_PROTOCOL_VERSION: u32 = 3;
/// senpi `coordinator-entry.ts`: the entry consumes the internal role (so descendants do not inherit it) and refuses
/// anything but an internal coordinator invocation.
pub fn require_coordinator_role(
    role: Option<crate::experimental::process::InternalProcessRole>,
) -> Result<(), String> {
    if role == Some(crate::experimental::process::InternalProcessRole::Coordinator) {
        Ok(())
    } else {
        Err("Coordinator entrypoint requires an internal coordinator invocation".to_owned())
    }
}
/// senpi `runCoordinatorProcess(args)`: both socket paths are required; runs until SIGINT/SIGTERM, which shut the
/// coordinator down and remove its sockets.
#[cfg(unix)]
pub async fn run_coordinator_process(args: &[String]) -> Result<(), String> {
    let [public, control, ..] = args else {
        return Err("Coordinator requires public and control socket paths".to_owned());
    };
    let shutdown = maho_ai::utils::abort::AbortController::new();
    let signal = shutdown.signal();
    let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
        .map_err(|error| error.to_string())?;
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .map_err(|error| error.to_string())?;
    let coordinator = run_coordinator(
        std::path::Path::new(public),
        std::path::Path::new(control),
        &signal,
    );
    tokio::pin!(coordinator);
    tokio::select! { result = &mut coordinator => result, _ = interrupt.recv() => { shutdown.abort(None); coordinator.await }, _ = terminate.recv() => { shutdown.abort(None); coordinator.await } }
}
/// senpi `coordinator-entry.ts` as one call: consume the role from the process environment, require `coordinator`,
/// then run the coordinator on its own runtime.
#[cfg(unix)]
pub fn run_coordinator_entry(args: &[String]) -> Result<(), String> {
    require_coordinator_role(crate::experimental::process::consume_process_internal_role()?)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| error.to_string())?;
    runtime.block_on(run_coordinator_process(args))
}
#[cfg(not(unix))]
pub fn run_coordinator_entry(_args: &[String]) -> Result<(), String> {
    Err("Coordinator named-pipe transport has not been ported on this platform".to_owned())
}
/// senpi `CoordinatorConnectionEvent`: what a registered server observes from its peers.
#[derive(Clone, Debug, PartialEq)]
pub enum CoordinatorConnectionEvent {
    PeerConnected {
        peer_id: String,
    },
    PeerDisconnected {
        peer_id: String,
    },
    Message {
        from: String,
        payload: serde_json::Value,
    },
}
#[cfg(unix)]
pub struct CoordinatorStartupLease(tokio::net::UnixStream);
#[cfg(unix)]
impl CoordinatorStartupLease {
    pub fn close(self) {
        drop(self.0);
    }
}
#[cfg(unix)]
pub async fn ensure_coordinator(
    public_path: &std::path::Path,
    control_path: &std::path::Path,
    cwd: &std::path::Path,
    env: &std::collections::BTreeMap<String, String>,
) -> Result<CoordinatorStartupLease, String> {
    async fn try_connect(path: &std::path::Path) -> Result<Option<tokio::net::UnixStream>, String> {
        match tokio::net::UnixStream::connect(path).await {
            Ok(socket) => Ok(Some(socket)),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
                ) =>
            {
                Ok(None)
            }
            Err(error) => Err(error.to_string()),
        }
    }
    if let Some(socket) = try_connect(control_path).await? {
        return Ok(CoordinatorStartupLease(socket));
    }
    let args = vec![
        public_path.to_string_lossy().into_owned(),
        control_path.to_string_lossy().into_owned(),
    ];
    let mut child = crate::experimental::process::spawn_internal_process(
        crate::experimental::process::InternalProcessRole::Coordinator,
        &args,
        cwd,
        env,
    )
    .map_err(|error| error.to_string())?;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        if let Some(socket) = try_connect(control_path).await? {
            return Ok(CoordinatorStartupLease(socket));
        }
        if child
            .try_wait()
            .map_err(|error| error.to_string())?
            .is_some()
        {
            return Err("Coordinator exited during startup".to_owned());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err("Timed out waiting for coordinator startup".to_owned());
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum CoordinatorMessage {
    #[serde(rename = "server_registered")]
    ServerRegistered {
        #[serde(rename = "serverConnectionId")]
        server_connection_id: String,
        peers: Vec<String>,
    },
    #[serde(rename = "server_replaced")]
    ServerReplaced,
    #[serde(rename = "peer_connected")]
    PeerConnected {
        #[serde(rename = "peerId")]
        peer_id: String,
    },
    #[serde(rename = "peer_disconnected")]
    PeerDisconnected {
        #[serde(rename = "peerId")]
        peer_id: String,
    },
    #[serde(rename = "message")]
    Message {
        from: String,
        payload: serde_json::Value,
    },
}
pub fn routed_messages(
    from: &str,
    message: &serde_json::Value,
    peers: &[String],
    server_present: bool,
) -> Result<Vec<(String, CoordinatorMessage)>, String> {
    match message["type"].as_str() {
        Some("send") => {
            let target = message["to"]
                .as_str()
                .filter(|target| !target.is_empty())
                .ok_or("Coordinator message target must be a string")?;
            if (target == "server" && server_present) || peers.iter().any(|peer| peer == target) {
                Ok(vec![(
                    target.to_owned(),
                    CoordinatorMessage::Message {
                        from: from.to_owned(),
                        payload: message["payload"].clone(),
                    },
                )])
            } else {
                Ok(Vec::new())
            }
        }
        Some("broadcast") => {
            if from != "server" {
                return Err("Only the current server may broadcast".to_owned());
            }
            Ok(peers
                .iter()
                .map(|peer| {
                    (
                        peer.clone(),
                        CoordinatorMessage::Message {
                            from: from.to_owned(),
                            payload: message["payload"].clone(),
                        },
                    )
                })
                .collect())
        }
        kind => Err(format!(
            "Unknown coordinator routing message: {}",
            kind.unwrap_or("undefined")
        )),
    }
}
