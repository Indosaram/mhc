//! Port of senpi packages/ai/src/cursor-agent-provider.ts.
// ported by todo 12

// Blocked: `stream`, `stream_simple`, and `fetch_cursor_usable_models` are not
// yet defined on `api::cursor_agent` (its main body is blocked on node
// 12-cursor-pb's generated protobuf types; see parity.d/12.md). Wiring this
// re-export now would break `cargo check -p maho-ai` for every lane sharing
// this crate.
// pub use crate::api::cursor_agent::{fetch_cursor_usable_models, stream, stream_simple};
