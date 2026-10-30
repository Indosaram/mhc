//! The supervisor's PUBLIC socket proxy (senpi `host-lifecycle-proxy.ts`): every client reaches the host through it.
//! The proxy is sealed-hop aware (senpi#3795): the internal side is connected with the supervisor's memory-only key,
//! and a POSIX public connection is filtered by its `RpcPublicEndpoint` before admission. Draining peers can still
//! probe and receive typed open refusals without a child hop.
#[cfg(unix)]
use crate::host_admission::HostAdmission;
#[cfg(unix)]
use crate::internal_channel::connect_internal_connection;
#[cfg(unix)]
use crate::rpc_public_key::RpcPublicEndpoint;
#[cfg(unix)]
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncWrite};

/// Either side of a hop once admitted: a sealed record stream, a filtered public view, or a raw socket.
pub trait HopStream: AsyncRead + AsyncWrite + Send + Unpin {}
impl<T: AsyncRead + AsyncWrite + Send + Unpin> HopStream for T {}
pub type BoxedHop = Box<dyn HopStream>;

/// One sealed readiness probe: the internal handshake and the ready reply together, on one monotonic timer
/// (senpi#3816 review). A child that accepts the hop and stays silent that long is not a working host.
pub const INTERNAL_READINESS_PROBE_MS: u64 = 10_000;

#[cfg(unix)]
#[derive(Clone)]
pub struct PublicProxyOptions {
    pub internal_socket: std::path::PathBuf,
    pub internal_key: Option<[u8; 32]>,
    pub public_endpoint: Option<Arc<RpcPublicEndpoint>>,
    pub require_token: bool,
    /// Shared authority for stopping, protocol replies and new-session admission.
    pub admission: Arc<HostAdmission>,
}

/// Opens the internal side of one public client: `None` while stopping, the sealed stream when a key is set.
#[cfg(unix)]
async fn open_internal(options: &PublicProxyOptions) -> std::io::Result<Option<BoxedHop>> {
    if options.admission.stopping() {
        return Ok(None);
    }
    let internal = tokio::net::UnixStream::connect(&options.internal_socket).await?;
    Ok(Some(match options.internal_key {
        // The proxy forwarding a public connection passes no deadline: a host whose loop stalls still serves the
        // connections made meanwhile.
        Some(key) => Box::new(
            connect_internal_connection(internal, &key, None)
                .await
                .map_err(|error| std::io::Error::other(error.0))?,
        ) as BoxedHop,
        None => Box::new(internal) as BoxedHop,
    }))
}

/// One accepted public client (senpi `createPublicProxy`'s connection handler).
#[cfg(unix)]
pub async fn proxy_public_client(
    client: tokio::net::UnixStream,
    options: PublicProxyOptions,
) -> std::io::Result<()> {
    let hop = open_internal(&options).await?;
    let client: BoxedHop = match &options.public_endpoint {
        Some(endpoint) => Box::new(crate::rpc_public_auth::public_rpc_stream(
            client,
            endpoint.clone(),
            options.require_token,
        )),
        None => Box::new(client),
    };
    crate::host_admission_proxy::proxy_with_admission(client, hop, options.admission.clone()).await;
    Ok(())
}

/// senpi `createPublicProxy` + its listener: accepts until draining, then waits for the clients it holds.
#[cfg(unix)]
pub async fn run_public_proxy(
    listener: tokio::net::UnixListener,
    options: PublicProxyOptions,
    mut draining: tokio::sync::watch::Receiver<bool>,
    connections: tokio::sync::watch::Sender<usize>,
) -> std::io::Result<()> {
    let mut clients = tokio::task::JoinSet::new();
    loop {
        if *draining.borrow() {
            break;
        }
        tokio::select! {
            biased;
            changed = draining.changed() => { if changed.is_err() || *draining.borrow() { break; } },
            result = clients.join_next(), if !clients.is_empty() => { let _ = result; connections.send_replace(clients.len()); },
            accepted = listener.accept() => {
                let (client, _) = accepted?;
                if *draining.borrow() { break; }
                clients.spawn(proxy_public_client(client, options.clone()));
                connections.send_replace(clients.len());
            }
        }
    }
    drop(listener);
    while clients.join_next().await.is_some() {
        connections.send_replace(clients.len());
    }
    Ok(())
}

/// What one readiness probe found (senpi `canConnect`).
#[derive(Debug, Clone, PartialEq)]
pub enum ReadinessProbe {
    /// The ready reply's `data`.
    Ready(serde_json::Value),
    /// Not listening yet: retried until the overall deadline.
    Refused,
    /// Accepted the hop and answered nothing within the probe bound.
    Silent,
    /// Answered but could not prove the key (`host_identity_unproven` / `auth_failed`): it will not start proving it.
    Unproven(&'static str),
}

/// One sealed readiness probe over the internal hop.
#[cfg(unix)]
pub async fn probe_internal_listener(
    socket_path: &std::path::Path,
    internal_key: &[u8; 32],
    probe_ms: u64,
) -> ReadinessProbe {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
    let Ok(socket) = tokio::net::UnixStream::connect(socket_path).await else {
        return ReadinessProbe::Refused;
    };
    let exchange = async {
        let mut stream = match connect_internal_connection(socket, internal_key, None).await {
            Ok(stream) => stream,
            Err(error) if matches!(error.0, "host_identity_unproven" | "auth_failed") => {
                return ReadinessProbe::Unproven(error.0);
            }
            Err(_) => return ReadinessProbe::Refused,
        };
        if stream
            .write_all(
                b"{\"id\":\"internal-ready\",\"type\":\"get_protocol_info\",\"observe\":true}\n",
            )
            .await
            .and(stream.flush().await)
            .is_err()
        {
            return ReadinessProbe::Refused;
        }
        let mut lines = tokio::io::BufReader::new(stream).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            let Ok(response) = serde_json::from_str::<serde_json::Value>(&line) else {
                return ReadinessProbe::Refused;
            };
            if response["id"] == "internal-ready" {
                return if response["success"] == true {
                    ReadinessProbe::Ready(response["data"].clone())
                } else {
                    ReadinessProbe::Refused
                };
            }
        }
        ReadinessProbe::Refused
    };
    // The connection was accepted: expiring now means the child is silent.
    tokio::time::timeout(std::time::Duration::from_millis(probe_ms), exchange)
        .await
        .unwrap_or(ReadinessProbe::Silent)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use serde_json::{Map, json};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
    const KEY: [u8; 32] = [7; 32];

    #[tokio::test]
    async fn a_sealed_host_is_ready_and_a_wrong_key_is_unproven() {
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("host.sock");
        let listener = tokio::net::UnixListener::bind(&path).expect("bind");
        tokio::spawn(async move {
            while let Ok((socket, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let Ok(stream) = crate::internal_channel::accept_internal_connection(
                        socket,
                        &KEY,
                        Map::new(),
                    )
                    .await
                    else {
                        return;
                    };
                    let (read, mut write) = tokio::io::split(stream);
                    let mut lines = tokio::io::BufReader::new(read).lines();
                    if let Ok(Some(_)) = lines.next_line().await {
                        let _ = write
                            .write_all(b"{\"id\":\"internal-ready\",\"type\":\"response\",\"success\":true,\"data\":{\"instanceId\":\"i\"}}\n")
                            .await;
                        let _ = write.flush().await;
                    }
                });
            }
        });
        assert_eq!(
            probe_internal_listener(&path, &KEY, 5000).await,
            ReadinessProbe::Ready(json!({"instanceId":"i"}))
        );
        assert_eq!(
            probe_internal_listener(&path, &[8; 32], 5000).await,
            ReadinessProbe::Unproven("host_identity_unproven")
        );
        assert_eq!(
            probe_internal_listener(&dir.path().join("absent"), &KEY, 5000).await,
            ReadinessProbe::Refused
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_child_that_accepts_and_never_answers_is_silent() {
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("host.sock");
        let listener = tokio::net::UnixListener::bind(&path).expect("bind");
        let held = tokio::spawn(async move { listener.accept().await.map(|(socket, _)| socket) });
        assert_eq!(
            probe_internal_listener(&path, &KEY, 1000).await,
            ReadinessProbe::Silent
        );
        drop(held);
    }

    #[tokio::test]
    async fn a_relay_cannot_inject_commands_after_authentication() {
        // senpi: a substituted socket relay cannot inject list_sessions after authentication; the hop is lost.
        let (client, host) = tokio::net::UnixStream::pair().expect("pair");
        let server = tokio::spawn(async move {
            crate::internal_channel::accept_internal_connection(host, &KEY, Map::new()).await
        });
        let client = connect_internal_connection(client, &KEY, Some(5000))
            .await
            .expect("client");
        let stream = server.await.expect("join").expect("server");
        let mut raw = client.into_inner_for_test();
        raw.write_all(b"{\"id\":\"forged\",\"type\":\"list_sessions\"}\n")
            .await
            .expect("inject");
        let mut lines = tokio::io::BufReader::new(stream).lines();
        assert!(lines.next_line().await.is_err());
    }
}
