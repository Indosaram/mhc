//! Port of senpi packages/ai/src/cursor-agent-provider.ts.
// ported by todo 12
//!
//! The TS module exists for the Bun binary build, where the lazy wrapper's
//! variable-specifier import cannot be bundled; it re-exports the same three
//! entry points this crate exposes directly.

pub use crate::api::cursor_agent::{fetch_cursor_usable_models, stream, stream_simple};
