//! Port of senpi packages/ai/src/api/websocket-liveness.ts.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub const WEBSOCKET_LIVENESS_PING_INTERVAL_MS: u64 = 30_000;
pub const WEBSOCKET_LIVENESS_PONG_TIMEOUT_MS: u64 = 20_000;
pub const WEBSOCKET_LIVENESS_MAX_UNANSWERED_PINGS: u32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WebSocketLivenessPolicy {
    pub ping_interval_ms: u64,
    pub pong_timeout_ms: u64,
    pub max_unanswered_pings: u32,
}

pub const DEFAULT_WEBSOCKET_LIVENESS_POLICY: WebSocketLivenessPolicy = WebSocketLivenessPolicy {
    ping_interval_ms: WEBSOCKET_LIVENESS_PING_INTERVAL_MS,
    pong_timeout_ms: WEBSOCKET_LIVENESS_PONG_TIMEOUT_MS,
    max_unanswered_pings: WEBSOCKET_LIVENESS_MAX_UNANSWERED_PINGS,
};

pub trait LivenessCapableSocket: Send + Sync {
    fn has_ping(&self) -> bool;
    fn ping(&self, data: &str) -> Result<(), String>;
}

pub fn format_websocket_liveness_failure(silent_ms: u64, unanswered_pings: u32) -> String {
    format!("WebSocket liveness timeout after {silent_ms}ms ({unanswered_pings} pings unanswered)")
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{}", format_websocket_liveness_failure(*silent_ms, *unanswered_pings))]
pub struct WebSocketLivenessError {
    pub silent_ms: u64,
    pub unanswered_pings: u32,
}

pub fn supports_websocket_liveness(socket: &dyn LivenessCapableSocket) -> bool {
    socket.has_ping()
}

pub struct WebSocketLivenessMonitor {
    activity: tokio::sync::mpsc::UnboundedSender<()>,
    stopped: Arc<AtomicBool>,
}

impl WebSocketLivenessMonitor {
    pub fn note_activity(&self) {
        let _ = self.activity.send(());
    }

    pub fn stop(&self) {
        self.stopped.store(true, Ordering::SeqCst);
        let _ = self.activity.send(());
    }
}

pub fn start_websocket_liveness(
    socket: Arc<dyn LivenessCapableSocket>,
    on_dead: impl Fn(WebSocketLivenessError) + Send + Sync + 'static,
    policy: WebSocketLivenessPolicy,
) -> WebSocketLivenessMonitor {
    let stopped = Arc::new(AtomicBool::new(false));
    let (activity, mut receiver) = tokio::sync::mpsc::unbounded_channel::<()>();

    if !socket.has_ping() {
        return WebSocketLivenessMonitor { activity, stopped };
    }
    let monitor_stopped = stopped.clone();

    let silent_since = Arc::new(Mutex::new(tokio::time::Instant::now()));
    let recorded_since = silent_since.clone();
    let watch = stopped.clone();
    tokio::spawn(async move {
        let stopped = watch;
        let mut unanswered_pings: u32 = 0;
        loop {
            if stopped.load(Ordering::SeqCst) {
                return;
            }
            let delay = if unanswered_pings == 0 { policy.ping_interval_ms } else { policy.pong_timeout_ms };
            let deadline = tokio::time::sleep(Duration::from_millis(delay));
            tokio::pin!(deadline);

            let mut saw_activity = false;
            tokio::select! {
                () = &mut deadline => {}
                event = receiver.recv() => {
                    if event.is_none() {
                        return;
                    }
                    saw_activity = true;
                }
            }

            if stopped.load(Ordering::SeqCst) {
                return;
            }

            if saw_activity {
                unanswered_pings = 0;
                *recorded_since.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) =
                    tokio::time::Instant::now();
                continue;
            }

            if unanswered_pings >= policy.max_unanswered_pings {
                let silent_ms = recorded_since
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .elapsed()
                    .as_millis() as u64;
                monitor_stopped.store(true, Ordering::SeqCst);
                on_dead(WebSocketLivenessError { silent_ms, unanswered_pings });
                return;
            }

            unanswered_pings += 1;
            if socket.ping("liveness").is_err() {
                let silent_ms = recorded_since
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .elapsed()
                    .as_millis() as u64;
                monitor_stopped.store(true, Ordering::SeqCst);
                on_dead(WebSocketLivenessError { silent_ms, unanswered_pings });
                return;
            }
        }
    });

    WebSocketLivenessMonitor { activity, stopped }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    struct Socket {
        pings: AtomicUsize,
    }

    impl LivenessCapableSocket for Socket {
        fn has_ping(&self) -> bool {
            true
        }

        fn ping(&self, _data: &str) -> Result<(), String> {
            self.pings.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    struct InertSocket;

    impl LivenessCapableSocket for InertSocket {
        fn has_ping(&self) -> bool {
            false
        }

        fn ping(&self, _data: &str) -> Result<(), String> {
            Err("no ping".into())
        }
    }

    #[test]
    fn failure_message_matches_the_ts_format() {
        assert_eq!(
            format_websocket_liveness_failure(120_000, 2),
            "WebSocket liveness timeout after 120000ms (2 pings unanswered)"
        );
        assert_eq!(
            WebSocketLivenessError { silent_ms: 5, unanswered_pings: 1 }.to_string(),
            "WebSocket liveness timeout after 5ms (1 pings unanswered)"
        );
    }

    #[test]
    fn sockets_without_ping_are_reported_as_unsupported() {
        assert!(supports_websocket_liveness(&Socket { pings: AtomicUsize::new(0) }));
        assert!(!supports_websocket_liveness(&InertSocket));
    }

    #[tokio::test(start_paused = true)]
    async fn unanswered_pings_declare_the_connection_dead_after_the_policy_budget() {
        let socket = Arc::new(Socket { pings: AtomicUsize::new(0) });
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let monitor = start_websocket_liveness(
            socket.clone(),
            move |error| {
                let _ = tx.send(error);
            },
            WebSocketLivenessPolicy { ping_interval_ms: 30, pong_timeout_ms: 10, max_unanswered_pings: 2 },
        );

        let mut error = None;
        for _ in 0..32 {
            tokio::time::advance(Duration::from_millis(10)).await;
            tokio::task::yield_now().await;
            if let Ok(dead) = rx.try_recv() {
                error = Some(dead);
                break;
            }
        }
        let error = error.expect("the monitor declares the connection dead");
        assert_eq!(error.unanswered_pings, 2);
        assert_eq!(socket.pings.load(Ordering::SeqCst), 2);
        monitor.stop();
    }

    #[tokio::test(start_paused = true)]
    async fn activity_resets_the_countdown() {
        let socket = Arc::new(Socket { pings: AtomicUsize::new(0) });
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let monitor = start_websocket_liveness(
            socket.clone(),
            move |error| {
                let _ = tx.send(error);
            },
            WebSocketLivenessPolicy { ping_interval_ms: 30, pong_timeout_ms: 10, max_unanswered_pings: 1 },
        );

        monitor.note_activity();
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(29)).await;
        tokio::task::yield_now().await;
        assert_eq!(socket.pings.load(Ordering::SeqCst), 0);
        tokio::time::advance(Duration::from_millis(1)).await;
        tokio::task::yield_now().await;
        assert_eq!(socket.pings.load(Ordering::SeqCst), 1);
        monitor.stop();
        tokio::time::advance(Duration::from_millis(50)).await;
        assert!(rx.try_recv().is_err());
    }
}
