//! Before a short-lived `host session` client closes, ask the host to drop its attachments and wait for
//! the answer (senpi `rpc/host-session-detach.ts`, senpi#3238). A host that does not answer within the
//! bound is reported in one plain line and the connection is closed anyway.
use crate::rpc_client::RpcClientError;
use std::future::Future;
use std::time::Duration;

pub const DETACH_ACK_TIMEOUT_MS: u64 = 5_000;

/// `detach` sends `detach_connection` on the client about to close.
pub async fn detach_before_close<T>(
    detach: impl Future<Output = Result<T, RpcClientError>>,
    mut report: impl FnMut(String),
    timeout_ms: u64,
) {
    match tokio::time::timeout(Duration::from_millis(timeout_ms), detach).await {
        Err(_) => report(format!(
            "the host did not confirm the detach within {timeout_ms} ms; closing the connection anyway"
        )),
        Ok(Ok(_)) => {}
        // A connection that is already gone holds nothing the host has not released on its close.
        Ok(Err(error)) if error.is_transport_gone() => {}
        Ok(Err(error)) => report(format!(
            "the host refused the detach ({error}); closing the connection anyway"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn a_silent_host_is_reported_and_a_gone_one_is_not() {
        let mut lines = Vec::new();
        detach_before_close(
            std::future::pending::<Result<(), RpcClientError>>(),
            |line| lines.push(line),
            50,
        )
        .await;
        assert_eq!(
            lines,
            vec![
                "the host did not confirm the detach within 50 ms; closing the connection anyway"
                    .to_owned()
            ]
        );
        lines.clear();
        let gone = async {
            Err::<(), _>(RpcClientError::Transport(
                std::io::ErrorKind::BrokenPipe.into(),
            ))
        };
        detach_before_close(gone, |line| lines.push(line), 50).await;
        assert!(lines.is_empty());
        detach_before_close(
            async { Ok::<_, RpcClientError>(()) },
            |line| lines.push(line),
            50,
        )
        .await;
        assert!(lines.is_empty());
    }
}
