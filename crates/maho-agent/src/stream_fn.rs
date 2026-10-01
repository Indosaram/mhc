//! Port of senpi packages/agent/src/stream-fn.ts.

use std::sync::RwLock;

use crate::types::StreamFn;

/// Error text senpi throws when no default stream function was configured.
pub const NO_DEFAULT_STREAM_FN: &str =
    "No default stream function configured. Pass streamFn explicitly or call setDefaultStreamFn().";

static DEFAULT_STREAM_FN: RwLock<Option<StreamFn>> = RwLock::new(None);

/// Configure the fallback used by Agent and low-level loops when callers omit streamFn.
///
/// Hosts that provide a default model runtime can install its stream function here without
/// making the agent crate depend on a provider catalog or compatibility layer.
pub fn set_default_stream_fn(stream_fn: Option<StreamFn>) {
    let mut slot = DEFAULT_STREAM_FN.write().unwrap_or_else(|poisoned| poisoned.into_inner());
    *slot = stream_fn;
}

/// The configured fallback. Panics with senpi's exact message when none is installed, which is
/// the Rust equivalent of the TS `throw new Error(...)`.
pub fn get_default_stream_fn() -> StreamFn {
    let slot = DEFAULT_STREAM_FN.read().unwrap_or_else(|poisoned| poisoned.into_inner());
    match slot.as_ref() {
        Some(stream_fn) => stream_fn.clone(),
        None => panic!("{NO_DEFAULT_STREAM_FN}"),
    }
}

pub use crate::empty_assistant_recovery::with_empty_assistant_recovery;
