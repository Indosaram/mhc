//! `dag/`: the DAG orchestration subsystem (slice C).

pub mod events;
pub mod execution_mode;
pub mod fingerprint;
pub mod graph;
pub mod handle;
pub mod journal;
pub mod manager;
pub mod node_activity;
pub mod node_control_context;
pub mod node_retry;
pub mod node_send;
pub mod owner;
pub mod recovery;
pub mod recovery_lease_watch;
pub mod results;
pub mod scheduler;
pub mod skills;
pub mod store;
#[cfg(test)]
pub(crate) mod test_support;
#[cfg(test)]
mod store_tests;
pub mod types;
#[cfg(test)]
mod e2e_tests;
