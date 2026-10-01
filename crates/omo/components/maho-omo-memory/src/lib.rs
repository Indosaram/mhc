//! Native memory component adapter over maho-memory-core.
pub mod binding;
pub mod context;
pub mod dream_scoring;
pub mod dream_selector;
pub mod dream_trigger_gates;
pub mod guard;
pub mod facts_run_cleanup;
pub mod prompt;
pub mod policy_guard;
pub mod skills_usage_ledger;
pub mod status_active_runs;
pub mod supervisor;
pub mod tool_receipts;
pub mod worker;

// Todo 45 owns these module bodies; declaring the namespaces here keeps lib.rs
// ownership with todo 43 without adding nonfunctional command implementations.
pub mod commands {}
pub mod palace {}
pub mod bindings {}
