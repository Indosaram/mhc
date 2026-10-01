//! Native memory component adapter over maho-memory-core.
pub mod binding;
pub mod context;
pub mod dream_scoring;
pub mod dream_selector;
pub mod dream_trigger_gates;
pub mod engine_session;
pub mod journal_wiring;
pub mod nudge_wiring;
pub mod guard;
pub mod facts_run_cleanup;
pub mod facts_failure_recording;
pub mod facts_launch_selection;
pub mod facts_oversize;
pub mod facts_people_payload;
pub mod facts_runner_types;
pub mod facts_run_storage;
pub mod facts_drain;
pub mod facts_terminal_writes;
pub mod prompt;
pub mod policy_guard;
pub mod skills_usage_ledger;
pub mod status;
pub mod status_active_runs;
pub mod supervisor;
pub mod tool_receipts;
pub mod worker;

// Todo 45 owns these module bodies; declaring the namespaces here keeps lib.rs
// ownership with todo 43 without adding nonfunctional command implementations.
pub mod commands {}
pub mod palace {}
pub mod bindings {}
