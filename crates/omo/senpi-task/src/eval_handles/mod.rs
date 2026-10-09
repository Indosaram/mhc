//! Eval-handle references and the host-side state machine.
pub mod control;
pub mod errors;
pub mod host;
pub mod index;
pub mod pool_refs;
pub mod run_after_engine;
pub mod steer_refs;
pub mod task_refs;
pub mod update_queue;
pub mod watch;

#[cfg(test)]
mod host_tests;

pub use host::{HandleKind, HandleOutcome, HandlePhase, HandleRef, HandleSnapshot};
