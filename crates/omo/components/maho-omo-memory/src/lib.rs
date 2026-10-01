//! Native memory component adapter over maho-memory-core.
pub mod binding;
pub mod context;
pub mod prompt;
pub mod status_active_runs;
pub mod supervisor;
pub mod worker;

// Todo 45 owns these module bodies; declaring the namespaces here keeps lib.rs
// ownership with todo 43 without adding nonfunctional command implementations.
pub mod commands {}
pub mod palace {}
pub mod bindings {}
