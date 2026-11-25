//! Port of senpi `packages/coding-agent/src/modes/rpc/session-event-writer-lanes.ts`.
//!
//! Shared stdio lane limits, actor drain helpers, and capacity guards.

pub use crate::session_event_writer::{
    MAX_SHARED_STDIO_QUEUE_BYTES, MAX_SHARED_STDIO_QUEUE_RECORDS,
};
use crate::socket_event_fanout::SocketEventSinkActor;

/// Awaits every actor's drain; a failed actor is a cut peer, not a writer failure.
pub async fn settle_actors(actors: &[&SocketEventSinkActor]) {
    for actor in actors {
        let mut actor = (*actor).clone();
        let _ = actor.flush().await;
    }
}

/// Checks whether appending `line` would exceed the shared stdio lane capacity.
pub fn exceeds_stdio_capacity(
    buffered_records: usize,
    buffered_bytes: usize,
    reserved_close_records: usize,
    reserved_close_bytes: usize,
    line: &str,
) -> bool {
    buffered_records + reserved_close_records >= MAX_SHARED_STDIO_QUEUE_RECORDS
        || buffered_bytes + reserved_close_bytes + line.len() > MAX_SHARED_STDIO_QUEUE_BYTES
}
