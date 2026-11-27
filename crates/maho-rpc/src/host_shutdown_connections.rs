//! The connection phase of a socket host's shutdown, after its sessions closed (senpi
//! `host-shutdown-connections.ts`).

use crate::host_stop_intent::CHILD_STOP_TIMEOUT_MS;
use futures::future::BoxFuture;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

/// RULING-124 (#5322): engine-initiated close codes for compact peers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SocketClosingCode {
    CheckpointUnavailable,
    EventQueueFailed,
    EventLeaseRefused,
    CompactEndpointUnsupported,
    HostShutdown,
    HostHandoff,
}

impl SocketClosingCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CheckpointUnavailable => "checkpoint_unavailable",
            Self::EventQueueFailed => "event_queue_failed",
            Self::EventLeaseRefused => "event_lease_refused",
            Self::CompactEndpointUnsupported => "compact_endpoint_unsupported",
            Self::HostShutdown => "host_shutdown",
            Self::HostHandoff => "host_handoff",
        }
    }

    pub const fn retryable(self) -> bool {
        !matches!(self, Self::CompactEndpointUnsupported)
    }
}

pub trait HostConnection: Send + Sync {
    fn id(&self) -> &str;
    fn detach(&self);
    fn close(&self, code: SocketClosingCode) -> BoxFuture<'static, ()>;
    fn is_destroyed(&self) -> bool;
    fn destroy(&self);
}

/// What the supervisor's SIGTERM-to-SIGKILL window (`CHILD_STOP_TIMEOUT_MS`) must keep after the
/// connection phase: the listener close, the socket unlink and the process exit.
const HOST_EXIT_RESERVE_MS: u64 = 3_000;

/// The deadline of a socket host's shutdown, counted from its start (the SIGTERM handler) to the
/// forced close of its last connection.
pub const HOST_SHUTDOWN_BUDGET_MS: u64 = CHILD_STOP_TIMEOUT_MS - HOST_EXIT_RESERVE_MS;

/// Cancels the forced socket destruction scheduled at the shutdown deadline.
pub struct ShutdownForceCancel {
    task: Option<tokio::task::JoinHandle<()>>,
}

impl ShutdownForceCancel {
    pub fn cancel(mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

async fn before_deadline(
    work: impl Future<Output = ()>,
    deadline_ms: u64,
    now_ms: &(dyn Fn() -> u64 + Send + Sync),
) {
    let remaining = deadline_ms.saturating_sub(now_ms());
    let _ = tokio::time::timeout(Duration::from_millis(remaining), work).await;
}

/// The connection phase of a socket host's shutdown, after its sessions closed: each connection
/// drains its own queue, then is announced and closed (`host_shutdown`), independently of the
/// others, so a peer that never reads holds only itself; then every connection detaches. All waits
/// share one deadline, `started_at_ms` plus `HOST_SHUTDOWN_BUDGET_MS`, and any socket still open
/// then is destroyed at it.
pub async fn close_host_connections<D, DFut>(
    connections: Vec<Arc<dyn HostConnection>>,
    drain: D,
    started_at_ms: u64,
    now_ms: Arc<dyn Fn() -> u64 + Send + Sync>,
    log: Arc<dyn Fn(String) + Send + Sync>,
) -> ShutdownForceCancel
where
    D: Fn(&str) -> DFut + Send + Sync,
    DFut: Future<Output = ()> + Send,
{
    let deadline = started_at_ms.saturating_add(HOST_SHUTDOWN_BUDGET_MS);
    let tasks = connections.iter().map(|connection| {
        let conn = connection.clone();
        let drain_fut = drain(conn.id());
        let now = now_ms.clone();
        async move {
            before_deadline(drain_fut, deadline, now.as_ref()).await;
            before_deadline(
                conn.close(SocketClosingCode::HostShutdown),
                deadline,
                now.as_ref(),
            )
            .await;
        }
    });
    futures::future::join_all(tasks).await;
    for connection in &connections {
        connection.detach();
    }
    let remaining = deadline.saturating_sub(now_ms());
    let task = tokio::runtime::Handle::try_current().ok().map(|handle| {
        handle.spawn(async move {
            tokio::time::sleep(Duration::from_millis(remaining)).await;
            let open: Vec<&Arc<dyn HostConnection>> =
                connections.iter().filter(|c| !c.is_destroyed()).collect();
            if open.is_empty() {
                return;
            }
            log(format!(
                "shutdown budget spent after {} ms; destroying {} open connection(s)",
                now_ms().saturating_sub(started_at_ms),
                open.len()
            ));
            for connection in open {
                connection.destroy();
            }
        })
    });
    ShutdownForceCancel { task }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::FutureExt;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

    struct MockConn {
        id: String,
        detached: AtomicBool,
        closed_code: std::sync::Mutex<Option<SocketClosingCode>>,
        destroyed: AtomicBool,
        hang_close: bool,
    }

    impl MockConn {
        fn new(id: &str, hang_close: bool) -> Arc<Self> {
            Arc::new(Self {
                id: id.into(),
                detached: AtomicBool::new(false),
                closed_code: std::sync::Mutex::new(None),
                destroyed: AtomicBool::new(false),
                hang_close,
            })
        }
    }

    impl HostConnection for MockConn {
        fn id(&self) -> &str {
            &self.id
        }

        fn detach(&self) {
            self.detached.store(true, Ordering::SeqCst);
        }

        fn close(&self, code: SocketClosingCode) -> BoxFuture<'static, ()> {
            *self
                .closed_code
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(code);
            if self.hang_close {
                std::future::pending().boxed()
            } else {
                self.destroyed.store(true, Ordering::SeqCst);
                std::future::ready(()).boxed()
            }
        }

        fn is_destroyed(&self) -> bool {
            self.destroyed.load(Ordering::SeqCst)
        }

        fn destroy(&self) {
            self.destroyed.store(true, Ordering::SeqCst);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn closes_healthy_connections_and_destroys_stalled_ones_at_budget() {
        let fast = MockConn::new("fast", false);
        let stalled = MockConn::new("stalled", true);
        let clock = Arc::new(AtomicU64::new(1000));
        let clock_fn = {
            let c = clock.clone();
            Arc::new(move || c.load(Ordering::SeqCst))
        };
        let logs = Arc::new(std::sync::Mutex::new(Vec::new()));
        let logs_sink = logs.clone();

        let cancel = close_host_connections(
            vec![fast.clone(), stalled.clone()],
            |_| async {},
            1000,
            clock_fn,
            Arc::new(move |msg| {
                logs_sink
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(msg);
            }),
        )
        .await;

        assert!(fast.detached.load(Ordering::SeqCst));
        assert!(stalled.detached.load(Ordering::SeqCst));
        assert!(fast.is_destroyed());
        assert!(!stalled.is_destroyed());
        assert_eq!(
            *fast
                .closed_code
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            Some(SocketClosingCode::HostShutdown)
        );

        // Advance virtual time past HOST_SHUTDOWN_BUDGET_MS and yield so the force task runs.
        tokio::task::yield_now().await;
        clock.store(1000 + HOST_SHUTDOWN_BUDGET_MS, Ordering::SeqCst);
        tokio::time::advance(Duration::from_millis(HOST_SHUTDOWN_BUDGET_MS + 1)).await;
        tokio::task::yield_now().await;
        assert!(stalled.is_destroyed());
        assert_eq!(
            logs.lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .len(),
            1
        );
        cancel.cancel();
    }
}
