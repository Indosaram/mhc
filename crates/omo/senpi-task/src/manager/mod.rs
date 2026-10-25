//! Task manager (`manager/` in TypeScript).

pub mod child_handle;
pub mod concurrency;
pub mod continue_result;
pub mod credential_failure;
pub mod depth_policy;
pub mod execution_mode;
pub mod helpers;
pub mod interrupted_turn;
pub mod isolation_wiring;
pub mod launch_fence;
#[expect(
    clippy::module_inception,
    reason = "mirrors the TS `manager/manager.ts` module path"
)]
pub mod manager;
pub mod names;
pub mod outcome;
pub mod parent_model_fallback;
pub mod parent_registry_context;
pub mod residency_signal;
pub mod respawn;
pub mod run_pause;
pub mod runner;
pub mod runtime_fallback_event;
pub mod start_admission;
pub mod transcript_log;
pub mod types;
pub mod workpool_admission;
#[cfg(test)]
pub(crate) mod workpool_admission_tests;

#[cfg(test)]
mod contract_tests;

#[cfg(test)]
pub(crate) mod manager_tests;

pub use child_handle::{ManagedChildHandle, ManagedChildListener, Unsubscribe};
pub use manager::{AbortSignal, TaskManager, WaitError, create_task_manager};
