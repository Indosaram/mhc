//! Child runners (`runners/` in TypeScript): in-process sessions and RPC child processes.

pub mod child_ancestry;
pub mod child_dialogs;
pub mod designated_chain_error;
pub mod designated_fallback_profile;
pub mod engine_spawned_by;
pub mod in_process;
pub mod input_admission_read;
pub mod input_effects;
pub mod parent_link;
pub mod pinned_model_equivalence;
pub mod remote_cloud;
pub mod retry_fallback_profile;
pub mod rpc;
pub mod rpc_host;
pub mod rpc_process;
pub mod task_parent_control;
pub mod types;

pub use in_process::child_handle::{RunnerFailure, RunnerFailureKind, RunnerOutcome};
