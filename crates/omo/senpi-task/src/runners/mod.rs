//! Child runners (`runners/` in TypeScript): in-process sessions and RPC child processes.

pub mod in_process;
pub mod pinned_model_equivalence;
pub mod rpc;
pub mod rpc_host;
pub mod rpc_process;
pub mod task_parent_control;
pub mod types;

pub use in_process::child_handle::{RunnerFailure, RunnerFailureKind, RunnerOutcome};
