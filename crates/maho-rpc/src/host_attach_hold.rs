//! The ensuring client's claim on a host between `ensure_host()` proving it ready and that client's own
//! attach (senpi `rpc/host-attach-hold.ts`). It is the very connection the readiness answer arrived on, kept
//! open, so the supervisor counts it as an attachment and never measures idle time across the gap.
//!
//! The claim ends when the caller releases it, and the operating system ends it when the ensuring process
//! dies, so an abandoned ensure never pins a host. The drain task never keeps the runtime alive at exit.
use tokio::io::AsyncReadExt;

pub struct HostAttachHold {
    drain: Option<tokio::task::JoinHandle<()>>,
}

impl HostAttachHold {
    /// Ends the claim. Idempotent; call it once the client's own connection is attached.
    pub fn release(&mut self) {
        if let Some(drain) = self.drain.take() {
            drain.abort();
        }
    }

    pub fn is_released(&self) -> bool {
        self.drain
            .as_ref()
            .is_none_or(tokio::task::JoinHandle::is_finished)
    }
}

impl Drop for HostAttachHold {
    fn drop(&mut self) {
        self.release();
    }
}

/// Keeps `stream` open as an attachment. A host broadcasts lifecycle records to every connection, so they
/// are drained and discarded and the pipe never backs up; an error or end of stream closes the hold.
pub fn hold_attachment<S>(mut stream: S) -> HostAttachHold
where
    S: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    let drain = tokio::spawn(async move {
        let mut buffer = [0u8; 8192];
        while matches!(stream.read(&mut buffer).await, Ok(size) if size > 0) {}
    });
    HostAttachHold { drain: Some(drain) }
}

/// senpi `holdProtocolInfo`: the protocol answer together with the hold of the connection it arrived on;
/// `None` (and the connection closed) when the host did not answer with a protocol.
#[cfg(unix)]
pub async fn hold_protocol_info(
    socket: &str,
    timeout_ms: u64,
) -> Option<(crate::host_protocol_info::HostProtocolInfo, HostAttachHold)> {
    let request = serde_json::json!({"id": "ensure-host-probe", "type": "get_protocol_info"});
    let (connection, answer) =
        crate::host_probe::authenticated_host_socket(socket, &request, timeout_ms, None)
            .await
            .ok()?;
    let data = answer
        .get("data")
        .filter(|_| answer["success"] == true)
        .cloned()
        .unwrap_or_else(|| serde_json::json!({}));
    let info = crate::host_protocol_info::parse_host_protocol_info(&data)?;
    let stream = connection.into_stream().ok()?;
    Some((info, hold_attachment(stream)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

    #[tokio::test]
    async fn the_hold_drains_broadcasts_until_released() {
        let (mut host, client) = tokio::io::duplex(16);
        let mut hold = hold_attachment(client);
        // More than the pipe's capacity: only a draining hold lets these writes finish.
        for _ in 0..64 {
            host.write_all(b"{\"type\":\"lifecycle\"}\n")
                .await
                .expect("drained");
        }
        hold.release();
        hold.release();
        let mut closed = false;
        for _ in 0..1_000 {
            if host.write_all(b"x").await.is_err() {
                closed = true;
                break;
            }
            tokio::task::yield_now().await;
        }
        assert!(closed, "released hold closes its end");
        assert!(hold.is_released());
    }

    #[tokio::test]
    async fn the_hold_ends_when_the_host_closes() {
        let (host, client) = tokio::io::duplex(16);
        let hold = hold_attachment(client);
        drop(host);
        let mut finished = false;
        for _ in 0..1_000 {
            if hold.is_released() {
                finished = true;
                break;
            }
            tokio::task::yield_now().await;
        }
        assert!(finished);
    }
}
