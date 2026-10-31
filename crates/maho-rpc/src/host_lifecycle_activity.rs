//! The supervisor's always-on observer connection to its host (senpi `host-lifecycle-activity.ts`
//! `SupervisorActivity.connectObserver`): the idle decision reads turns from it. On POSIX it connects through the
//! sealed internal hop with a bounded handshake; a child that accepts and never answers fails the open, and the
//! connection is dropped.
#[cfg(unix)]
use crate::host_lifecycle_proxy::BoxedHop;

/// The observer's internal handshake bound (senpi#3816 review), the same 5 s the raw connect had.
pub const INTERNAL_OBSERVER_HANDSHAKE_MS: u64 = 5_000;

/// Opens one observer connection: sealed with `internal_key` when set, else a raw connect bounded by the same 5 s.
/// On failure the socket is dropped (destroyed).
#[cfg(unix)]
pub async fn connect_observer(
    internal_socket: &std::path::Path,
    internal_key: Option<&[u8; 32]>,
) -> std::io::Result<BoxedHop> {
    let bound = std::time::Duration::from_millis(INTERNAL_OBSERVER_HANDSHAKE_MS);
    let connect = tokio::net::UnixStream::connect(internal_socket);
    match internal_key {
        Some(key) => {
            // The handshake bound covers the connect too: one monotonic timer for the whole open.
            let sealed = tokio::time::timeout(bound, async {
                let socket = connect.await?;
                crate::internal_channel::connect_internal_connection(socket, key, None)
                    .await
                    .map_err(|error| std::io::Error::other(error.0))
            })
            .await
            .map_err(|_| std::io::Error::other("internal_auth_timeout"))??;
            Ok(Box::new(sealed))
        }
        None => {
            let socket = tokio::time::timeout(bound, connect).await.map_err(|_| {
                std::io::Error::other(format!(
                    "observer connection to internal host timed out after {INTERNAL_OBSERVER_HANDSHAKE_MS}ms"
                ))
            })??;
            Ok(Box::new(socket))
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    #[tokio::test(start_paused = true)]
    async fn a_child_that_accepts_and_never_answers_fails_the_open_within_the_bound() {
        let dir = tempfile::tempdir().expect("dir");
        let path = dir.path().join("host.sock");
        let listener = tokio::net::UnixListener::bind(&path).expect("bind");
        let held = tokio::spawn(async move { listener.accept().await.map(|(socket, _)| socket) });
        let started = tokio::time::Instant::now();
        let error = connect_observer(&path, Some(&[7; 32]))
            .await
            .err()
            .expect("timed out");
        assert_eq!(error.to_string(), "internal_auth_timeout");
        assert_eq!(
            started.elapsed(),
            std::time::Duration::from_millis(INTERNAL_OBSERVER_HANDSHAKE_MS)
        );
        drop(held);
    }
}
