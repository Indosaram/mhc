//! Workpool contracts (`workpool/` in TypeScript).

pub mod aggregate;
pub mod default_mode;
pub mod dispatcher;
pub mod engine;
pub mod measurement;
#[cfg(test)]
mod ported_tests;
pub mod ports;
pub mod process_worker;
pub mod reconcile;
pub mod record_schema;
pub mod run_pause;
pub mod schema;
pub mod store;
pub mod types;
pub mod worker_capability;
pub mod worker_kernel_tools;
pub mod worker_tool_identity;
pub mod worker_turn;
pub mod yield_;
pub mod yield_event;

#[cfg(test)]
mod run_pause_tests;
