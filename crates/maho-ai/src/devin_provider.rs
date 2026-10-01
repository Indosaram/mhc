//! Port of senpi packages/ai/src/devin-provider.ts.
//!
//! The TS module exists for the standalone Bun binary, where the lazy wrapper's variable-specifier
//! import cannot be bundled; it re-exports the two stream entry points this crate exposes directly.

pub use crate::api::devin_agent::{stream, stream_simple};
