//! The engine side of the supervisor's private hop (senpi `host-internal-transport.ts`, senpi#3795): the
//! stdin-delivered key and lifetime descriptor from the bootstrap, and how each accepted connection is admitted.
use crate::host_lifecycle_proxy::BoxedHop;
use crate::host_watchdog::HostWatchdogConfig;
#[cfg(unix)]
use crate::rpc_public_key::RpcPublicEndpoint;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

static BINDING: OnceLock<Option<HostWatchdogConfig>> = OnceLock::new();

/// senpi `hostWatchdogBinding`: the supervisor binding, read once per process and removed from the snapshot as it
/// is read (senpi#3815 review L4). The process environment itself is scrubbed by the CLI entry.
pub fn host_watchdog_binding() -> Option<HostWatchdogConfig> {
    BINDING
        .get_or_init(|| {
            let mut env: HashMap<String, String> = std::env::vars().collect();
            crate::host_watchdog::consume_host_watchdog_env(&mut env)
        })
        .clone()
}

pub struct InternalHostTransport {
    pub watchdog_config: Option<HostWatchdogConfig>,
    key: Option<[u8; 32]>,
    secret: Option<Arc<Vec<u8>>>,
    #[cfg(unix)]
    public_endpoint: Option<Arc<RpcPublicEndpoint>>,
    #[cfg(unix)]
    require_token: bool,
}

/// senpi `internalHostTransport`. Supervised means holding the internal key, which only the launching supervisor
/// can hand over and no descendant inherits; a host without it is a public endpoint whatever its environment says.
pub fn internal_host_transport(
    configured: Option<HostWatchdogConfig>,
    secret: Option<Arc<Vec<u8>>>,
    endpoint: Option<(&std::path::Path, &str)>,
) -> std::io::Result<InternalHostTransport> {
    // senpi runs the bootstrap at import time; the CLI entry should call it first (and scrub the marker). A socket
    // host reads nothing from stdin, so running it here, once, is equivalent when the entry has not.
    crate::internal_bootstrap::init_internal_host_bootstrap(&std::env::vars().collect())
        .map_err(std::io::Error::other)?;
    let bootstrap = crate::internal_bootstrap::get_internal_host_bootstrap();
    let watchdog_config = configured.map(|mut config| {
        if let Some(fd) = bootstrap.and_then(|bootstrap| bootstrap.lifetime_fd) {
            config.lifetime_fd = Some(fd);
        }
        config
    });
    #[cfg(unix)]
    let public_endpoint = match endpoint {
        Some((agent_dir, socket)) if bootstrap.is_none() => Some(Arc::new(
            RpcPublicEndpoint::new(agent_dir, socket).map_err(std::io::Error::other)?,
        )),
        _ => None,
    };
    #[cfg(not(unix))]
    let _ = endpoint;
    Ok(InternalHostTransport {
        watchdog_config,
        key: bootstrap.map(|bootstrap| bootstrap.key),
        secret,
        #[cfg(unix)]
        public_endpoint,
        #[cfg(unix)]
        require_token: crate::rpc_public_auth::PublicRpcConnection::require_token_from_env(),
    })
}

impl InternalHostTransport {
    /// Authenticates and seals (POSIX key), authenticates (Windows secret), filters (public), or accepts as is.
    /// `None` is a refused connection, already logged and closed.
    pub async fn admit<S>(&self, mut socket: S) -> Option<BoxedHop>
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin + 'static,
    {
        if let Some(key) = &self.key {
            let mut protocol = serde_json::Map::new();
            protocol.insert("protocolVersion".into(), 1.into());
            protocol.insert(
                "serverVersion".into(),
                maho_core::engine_build_identity::engine_build_identity()
                    .text
                    .clone()
                    .into(),
            );
            return match crate::internal_channel::accept_internal_connection(socket, key, protocol)
                .await
            {
                Ok(stream) => Some(Box::new(stream) as BoxedHop),
                Err(error) => {
                    // One line per refused connection: the refusal code only.
                    eprintln!("senpi rpc host: internal connection refused: {}", error.0);
                    None
                }
            };
        }
        if let Some(secret) = &self.secret {
            // A wrong or missing secret ends the connection before any JSONL byte is read or answered.
            return crate::socket_transport::authenticate_socket(&mut socket, secret)
                .await
                .ok()
                .map(|()| Box::new(socket) as BoxedHop);
        }
        #[cfg(unix)]
        if let Some(endpoint) = &self.public_endpoint {
            return Some(Box::new(crate::rpc_public_auth::public_rpc_stream(
                socket,
                endpoint.clone(),
                self.require_token,
            )));
        }
        Some(Box::new(socket))
    }

    /// Publishes the public endpoint's key record after listening. A failure is logged and returned: the host ends
    /// (held connections are refused, never served keyless).
    #[cfg(not(unix))]
    pub fn publish(&self) -> std::io::Result<()> {
        Ok(())
    }
    #[cfg(not(unix))]
    pub fn dispose(&self) {}

    #[cfg(unix)]
    pub fn publish(&self) -> std::io::Result<()> {
        let Some(endpoint) = &self.public_endpoint else {
            return Ok(());
        };
        endpoint.publish().map_err(|error| {
            eprintln!("senpi rpc host: {error}");
            std::io::Error::other(error)
        })
    }

    #[cfg(unix)]
    pub fn dispose(&self) {
        if let Some(endpoint) = &self.public_endpoint
            && let Err(error) = endpoint.dispose()
        {
            eprintln!("senpi rpc host: endpoint key cleanup failed: {error}");
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    #[tokio::test]
    async fn a_host_without_key_or_secret_or_endpoint_accepts_as_is() {
        let transport = internal_host_transport(None, None, None).expect("transport");
        let (socket, _peer) = tokio::io::duplex(64);
        assert!(transport.admit(socket).await.is_some());
    }
    #[tokio::test]
    async fn a_wrong_secret_is_refused() {
        use tokio::io::AsyncWriteExt;
        let transport =
            internal_host_transport(None, Some(Arc::new(vec![1; 32])), None).expect("transport");
        let (socket, mut peer) = tokio::io::duplex(64);
        peer.write_all(&[2; 32]).await.expect("w");
        assert!(transport.admit(socket).await.is_none());
    }

    fn sealed(key: [u8; 32]) -> InternalHostTransport {
        InternalHostTransport {
            watchdog_config: None,
            key: Some(key),
            secret: None,
            public_endpoint: None,
            require_token: false,
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_keyed_host_admits_a_sealed_round_trip_and_refuses_another_key() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let transport = sealed([7; 32]);
        let (socket, peer) = tokio::io::duplex(4096);
        let client =
            crate::internal_channel::connect_internal_connection(peer, &[7; 32], Some(5000));
        let (hop, client) = tokio::join!(transport.admit(socket), client);
        let mut hop = hop.expect("admitted");
        let mut client = client.expect("sealed client");
        client.write_all(b"{\"type\":\"ping\"}\n").await.expect("w");
        client.flush().await.expect("f");
        let mut request = [0u8; 16];
        hop.read_exact(&mut request).await.expect("r");
        assert_eq!(&request, b"{\"type\":\"ping\"}\n");
        hop.write_all(b"pong\n").await.expect("w");
        hop.flush().await.expect("f");
        let mut reply = [0u8; 5];
        client.read_exact(&mut reply).await.expect("r");
        assert_eq!(&reply, b"pong\n");

        let (socket, peer) = tokio::io::duplex(4096);
        let client =
            crate::internal_channel::connect_internal_connection(peer, &[8; 32], Some(5000));
        let (hop, client) = tokio::join!(transport.admit(socket), client);
        assert!(hop.is_none());
        assert_eq!(client.err().expect("refused").0, "host_identity_unproven");
    }
}
