//! Child runners (`runners/` in TypeScript): in-process sessions and RPC child processes.

pub mod in_process;
pub mod rpc;
pub mod rpc_process;
pub mod types;

pub use in_process::child_handle::{RunnerFailure, RunnerFailureKind, RunnerOutcome};
