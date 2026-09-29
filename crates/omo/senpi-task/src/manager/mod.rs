//! Task manager (`manager/` in TypeScript).

pub mod child_handle;
pub mod concurrency;
pub mod continue_result;
pub mod depth_policy;
pub mod execution_mode;
pub mod helpers;
pub mod interrupted_turn;
#[expect(
    clippy::module_inception,
    reason = "mirrors the TS `manager/manager.ts` module path"
)]
pub mod manager;
pub mod names;
pub mod outcome;
pub mod parent_registry_context;
pub mod respawn;
pub mod runner;
pub mod runtime_fallback_event;
pub mod transcript_log;
pub mod types;

#[cfg(test)]
mod manager_tests;

pub use child_handle::{ManagedChildHandle, ManagedChildListener, Unsubscribe};
pub use manager::{AbortSignal, TaskManager, WaitError, create_task_manager};
