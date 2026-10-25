//! `dag/`: the DAG orchestration subsystem (slice C).

#[cfg(test)]
mod e2e_tests;
pub mod events;
pub mod execution_mode;
pub mod fingerprint;
pub mod graph;
pub mod handle;
#[cfg(test)]
mod infra_retry_fixture;
pub mod infrastructure_retry;
pub mod journal;
pub mod manager;
pub mod node_activity;
pub mod node_control_context;
pub mod node_retry;
pub mod node_send;
pub mod owner;
pub mod paused_child_recovery;
pub mod paused_controls;
#[cfg(test)]
mod paused_workflow_tests;
pub mod recovery;
pub mod recovery_lease_watch;
pub mod results;
pub mod scheduler;
pub mod skills;
pub mod store;
#[cfg(test)]
mod store_tests;
#[cfg(test)]
pub(crate) mod test_support;
pub mod types;
// getrusage-based CPU timing is Unix-only.
#[cfg(all(test, unix))]
mod perf_bench;
