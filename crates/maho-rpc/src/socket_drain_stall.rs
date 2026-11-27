//! Port of senpi `packages/coding-agent/src/modes/rpc/socket-drain-stall.ts`.
//!
//! Resolves when the sink accepts the write, or fails with `SocketEventQueueStallError` once the
//! peer has held the transport full for `stall_ms`.

use crate::loop_blocked_time::LoopBlockedTime;
use crate::socket_event_queue_errors::SocketEventQueueStallError;
use futures::future::BoxFuture;
use maho_ai::utils::abort::AbortSignal;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Debug, thiserror::Error)]
pub enum DrainStallError {
    #[error(transparent)]
    Stall(#[from] SocketEventQueueStallError),
    #[error("{0}")]
    Transport(String),
}

pub trait DrainStallSink: Send + Sync {
    fn write_raw(&self, chunk: &str) -> Result<(), String>;
    fn wait_for_backpressure(&self) -> BoxFuture<'static, Result<(), String>>;
}

pub struct DrainStallPeer {
    pub sink: Arc<dyn DrainStallSink>,
    pub stall_ms: u64,
    pub stopped: AbortSignal,
    pub queued_bytes: Arc<dyn Fn() -> usize + Send + Sync>,
    /// A resync-capable peer sheds and pings at the deadline instead of being cut; false for a
    /// plain legacy peer.
    pub shed_and_ping: Arc<dyn Fn() -> Result<bool, String> + Send + Sync>,
    pub blocked_time: Arc<Mutex<LoopBlockedTime>>,
}

const STALL_NOTICE: &str = "{\"type\":\"overflow\",\"error\":\"stalled, resync required\"}\n";

pub async fn wait_for_drain_or_stall(
    peer: &DrainStallPeer,
    written_bytes: usize,
) -> Result<(), DrainStallError> {
    if peer.stopped.aborted() {
        return Ok(());
    }
    let mut blocked_mark = peer
        .blocked_time
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .loop_blocked_mark();
    let mut next_wait = Duration::from_millis(peer.stall_ms);
    let mut backpressure = peer.sink.wait_for_backpressure();

    loop {
        tokio::select! {
            biased;
            () = peer.stopped.cancelled() => return Ok(()),
            res = &mut backpressure => return res.map_err(DrainStallError::Transport),
            () = tokio::time::sleep(next_wait) => {
                let blocked_ms = {
                    let guard = peer
                        .blocked_time
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    let delta = guard.loop_blocked_ms_since(blocked_mark);
                    if delta > 0.0 {
                        blocked_mark = guard.loop_blocked_mark();
                    }
                    delta
                };
                if blocked_ms > 0.0 {
                    let ms = blocked_ms.min(peer.stall_ms as f64).max(1.0) as u64;
                    next_wait = Duration::from_millis(ms);
                    continue;
                }
                match (peer.shed_and_ping)() {
                    Ok(true) => {
                        blocked_mark = peer
                            .blocked_time
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .loop_blocked_mark();
                        next_wait = Duration::from_millis(peer.stall_ms);
                        continue;
                    }
                    Ok(false) => {}
                    Err(err) => return Err(DrainStallError::Transport(err)),
                }
                let stall = SocketEventQueueStallError {
                    pending_bytes: (peer.queued_bytes)() + written_bytes,
                    stall_ms: peer.stall_ms,
                };
                let _ = peer.sink.write_raw(STALL_NOTICE);
                return Err(DrainStallError::Stall(stall));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use maho_ai::utils::abort::AbortController;
    use std::sync::atomic::{AtomicBool, Ordering};

    struct TestSink {
        backpressure: Mutex<Option<tokio::sync::oneshot::Receiver<Result<(), String>>>>,
        notices: Mutex<Vec<String>>,
    }

    impl DrainStallSink for TestSink {
        fn write_raw(&self, chunk: &str) -> Result<(), String> {
            self.notices
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(chunk.to_owned());
            Ok(())
        }

        fn wait_for_backpressure(&self) -> BoxFuture<'static, Result<(), String>> {
            let rx = self
                .backpressure
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
            Box::pin(async move {
                match rx {
                    Some(rx) => rx.await.unwrap_or(Ok(())),
                    None => std::future::pending().await,
                }
            })
        }
    }

    #[tokio::test(start_paused = true)]
    async fn stall_writes_notice_and_returns_stall_error() {
        let (_tx, rx) = tokio::sync::oneshot::channel();
        let sink = Arc::new(TestSink {
            backpressure: Mutex::new(Some(rx)),
            notices: Mutex::new(Vec::new()),
        });
        let abort = AbortController::new();
        let peer = DrainStallPeer {
            sink: sink.clone(),
            stall_ms: 500,
            stopped: abort.signal(),
            queued_bytes: Arc::new(|| 128),
            shed_and_ping: Arc::new(|| Ok(false)),
            blocked_time: Arc::new(Mutex::new(LoopBlockedTime::default())),
        };
        let err = wait_for_drain_or_stall(&peer, 64).await.unwrap_err();
        assert!(matches!(
            err,
            DrainStallError::Stall(SocketEventQueueStallError {
                pending_bytes: 192,
                stall_ms: 500,
            })
        ));
        assert_eq!(
            *sink
                .notices
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            vec![STALL_NOTICE]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn shed_and_ping_extends_deadline_until_drain() {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let sink = Arc::new(TestSink {
            backpressure: Mutex::new(Some(rx)),
            notices: Mutex::new(Vec::new()),
        });
        let abort = AbortController::new();
        let shed_called = Arc::new(AtomicBool::new(false));
        let shed_flag = shed_called.clone();
        let tx_cell = Arc::new(Mutex::new(Some(tx)));
        let peer = DrainStallPeer {
            sink,
            stall_ms: 500,
            stopped: abort.signal(),
            queued_bytes: Arc::new(|| 0),
            shed_and_ping: Arc::new(move || {
                shed_flag.store(true, Ordering::SeqCst);
                if let Some(sender) = tx_cell
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .take()
                {
                    let _ = sender.send(Ok(()));
                }
                Ok(true)
            }),
            blocked_time: Arc::new(Mutex::new(LoopBlockedTime::default())),
        };
        wait_for_drain_or_stall(&peer, 10).await.expect("drained");
        assert!(shed_called.load(Ordering::SeqCst));
    }
}
