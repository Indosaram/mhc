//! Port of senpi `packages/coding-agent/src/modes/rpc/socket-event-queue-errors.ts`.

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "socket event queue stalled: peer did not drain {pending_bytes} queued bytes within {stall_ms}ms"
)]
pub struct SocketEventQueueStallError {
    pub pending_bytes: usize,
    pub stall_ms: u64,
}

impl SocketEventQueueStallError {
    pub const NAME: &'static str = "SocketEventQueueStallError";
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "socket event queue overflow: {queued_bytes} queued + {incoming_bytes} incoming > {max_queue_bytes} (incoming: {incoming_preview})"
)]
pub struct SocketEventQueueOverflowError {
    pub queued_bytes: usize,
    pub incoming_bytes: usize,
    pub max_queue_bytes: usize,
    pub incoming_preview: String,
}

impl SocketEventQueueOverflowError {
    pub const NAME: &'static str = "SocketEventQueueOverflowError";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_messages_match_upstream_format() {
        let stall = SocketEventQueueStallError {
            pending_bytes: 512,
            stall_ms: 30_000,
        };
        assert_eq!(
            stall.to_string(),
            "socket event queue stalled: peer did not drain 512 queued bytes within 30000ms"
        );
        let overflow = SocketEventQueueOverflowError {
            queued_bytes: 100,
            incoming_bytes: 50,
            max_queue_bytes: 120,
            incoming_preview: "{\"type\":\"x\"}".into(),
        };
        assert_eq!(
            overflow.to_string(),
            "socket event queue overflow: 100 queued + 50 incoming > 120 (incoming: {\"type\":\"x\"})"
        );
    }
}
