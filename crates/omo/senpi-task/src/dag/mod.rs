//! `dag/`: the DAG orchestration subsystem (slice C).

pub mod events;
pub mod execution_mode;
pub mod fingerprint;
pub mod graph;
pub mod handle;
pub mod journal;
pub mod manager;
pub mod owner;
pub mod recovery;
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
