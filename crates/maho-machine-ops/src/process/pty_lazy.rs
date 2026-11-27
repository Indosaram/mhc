//! Port of `packages/machine-ops/src/process/pty.lazy.ts`.
//!
//! In Rust `maho-pty` is statically linked without heavyweight JS DOM/headless imports; this module
//! preserves the module boundary and constructor helper so `in_process_holder.rs` loads sessions
//! lazily on first spawn.

use maho_pty::{PtyError, PtySession, PtySessionOptions};
use std::sync::Arc;

pub type PtyChunkHandler = Arc<dyn Fn(&[u8]) + Send + Sync>;

pub(crate) fn spawn_pty_session(
    opts: PtySessionOptions,
    on_chunk: PtyChunkHandler,
) -> Result<PtySession, PtyError> {
    PtySession::start(opts, move |chunk| on_chunk(chunk))
}
