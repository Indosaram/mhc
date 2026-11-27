//! Port of senpi `packages/coding-agent/src/modes/rpc/socket-closing.ts`.
//!
//! RULING-124 (#5322): the engine never closes a compact peer's socket without saying why.

use crate::jsonl::serialize_json_line;
use crate::socket_event_queue_errors::{SocketEventQueueOverflowError, SocketEventQueueStallError};
use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::error::Error;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

const SOCKET_CLOSING_FLUSH: Duration = Duration::from_millis(1_000);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcSocketClosingEvent {
    #[serde(rename = "type")]
    pub record_type: String,
    pub code: SocketClosingCode,
    pub retryable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl RpcSocketClosingEvent {
    pub fn new(code: SocketClosingCode, detail: Option<String>) -> Self {
        Self {
            record_type: "socket_closing".to_owned(),
            code,
            retryable: code.retryable(),
            detail,
        }
    }
}

pub trait ClosingTransport: Send + Sync {
    /// Stable identity key for idempotent close deduplication per transport.
    fn identity(&self) -> usize;
    fn write_raw(&self, chunk: &str) -> Result<(), String>;
    fn wait_for_backpressure(&self) -> BoxFuture<'static, Result<(), String>>;
    fn close(&self);
}

pub trait CloseableQueue {
    fn close(&self);
}

fn closing_transports() -> &'static Mutex<HashSet<usize>> {
    static SET: OnceLock<Mutex<HashSet<usize>>> = OnceLock::new();
    SET.get_or_init(Mutex::default)
}

/// The one way the engine closes a peer's socket.
pub async fn close_socket<Q: CloseableQueue + ?Sized>(
    transport: &dyn ClosingTransport,
    queue: &Q,
    compact: bool,
    code: SocketClosingCode,
    detail: Option<String>,
) {
    if !compact {
        transport.close();
        return;
    }
    let id = transport.identity();
    {
        let mut set = closing_transports()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !set.insert(id) {
            return;
        }
    }
    queue.close();
    let event = RpcSocketClosingEvent::new(code, detail);
    let write_res = serialize_json_line(&event)
        .map_err(|e| e.to_string())
        .and_then(|line| transport.write_raw(&line));
    let failure = match write_res {
        Err(err) => Some(err),
        Ok(()) => {
            match tokio::time::timeout(SOCKET_CLOSING_FLUSH, transport.wait_for_backpressure())
                .await
            {
                Ok(Ok(())) => None,
                Ok(Err(err)) => Some(err),
                Err(_) => None,
            }
        }
    };
    if let Some(failure) = failure {
        eprintln!(
            "senpi rpc socket_closing ({}) not delivered: {failure}; closing",
            code.as_str()
        );
    }
    transport.close();
    closing_transports()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(&id);
}

/// The `detail` of an `event_queue_failed` close: which of the queue's failures ended it.
pub fn queue_failure_detail(cause: &(dyn Error + 'static)) -> &'static str {
    if cause
        .downcast_ref::<SocketEventQueueOverflowError>()
        .is_some()
    {
        return "overflow";
    }
    if cause.downcast_ref::<SocketEventQueueStallError>().is_some() {
        return "stall";
    }
    "transport_error"
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    struct TestTransport {
        id: usize,
        written: Mutex<Vec<String>>,
        closed: AtomicBool,
    }

    impl ClosingTransport for TestTransport {
        fn identity(&self) -> usize {
            self.id
        }
        fn write_raw(&self, chunk: &str) -> Result<(), String> {
            self.written
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(chunk.to_owned());
            Ok(())
        }
        fn wait_for_backpressure(&self) -> BoxFuture<'static, Result<(), String>> {
            Box::pin(async { Ok(()) })
        }
        fn close(&self) {
            self.closed.store(true, Ordering::SeqCst);
        }
    }

    struct TestQueue(Arc<AtomicBool>);
    impl CloseableQueue for TestQueue {
        fn close(&self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    #[tokio::test]
    async fn compact_close_writes_socket_closing_and_non_compact_writes_nothing() {
        let q_closed = Arc::new(AtomicBool::new(false));
        let t_legacy = TestTransport {
            id: 9001,
            written: Mutex::new(Vec::new()),
            closed: AtomicBool::new(false),
        };
        close_socket(
            &t_legacy,
            &TestQueue(q_closed.clone()),
            false,
            SocketClosingCode::HostShutdown,
            None,
        )
        .await;
        assert!(t_legacy.closed.load(Ordering::SeqCst));
        assert!(!q_closed.load(Ordering::SeqCst));
        assert!(
            t_legacy
                .written
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_empty()
        );

        let t_compact = TestTransport {
            id: 9002,
            written: Mutex::new(Vec::new()),
            closed: AtomicBool::new(false),
        };
        close_socket(
            &t_compact,
            &TestQueue(q_closed.clone()),
            true,
            SocketClosingCode::CompactEndpointUnsupported,
            Some("legacy_only".into()),
        )
        .await;
        assert!(q_closed.load(Ordering::SeqCst));
        assert!(t_compact.closed.load(Ordering::SeqCst));
        let lines = t_compact
            .written
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        assert_eq!(lines.len(), 1);
        let parsed: serde_json::Value = serde_json::from_str(&lines[0]).expect("json");
        assert_eq!(parsed["type"], "socket_closing");
        assert_eq!(parsed["code"], "compact_endpoint_unsupported");
        assert_eq!(parsed["retryable"], false);
        assert_eq!(parsed["detail"], "legacy_only");
    }
}
