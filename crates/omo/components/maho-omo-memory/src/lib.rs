//! Native memory component adapter over maho-memory-core.
pub mod binding;
pub mod capabilities;
pub mod context;
pub mod dream_scoring;
pub mod dream_selector;
pub mod dream_trigger_gates;
pub mod dream_trigger_fire;
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
pub mod facts_wiring;
pub mod facts_run_storage;
pub mod facts_run_prune;
pub mod facts_run_reconcile;
pub mod facts_run_finalize;
pub mod facts_drain;
pub mod facts_batch_apply;
pub mod facts_terminal_writes;
pub mod prompt;
pub mod reflection_settings;
pub mod session_model_resolver;
pub mod model_registry_resolver;
pub mod session_context_resolver;
pub mod shutdown_drain;
pub mod memory_rpc_snapshot_state;
pub mod memory_rpc_bridge;
pub mod policy_guard;
pub mod skills_usage_ledger;
pub mod skills_usage;
pub mod skills_usage_tracker;
pub mod skills_usage_wiring;
pub mod skills_scope;
pub mod status;
pub mod status_live;
pub mod status_live_wiring;
pub mod status_active_runs;
pub mod supervisor;
pub mod soul_notice;
pub mod sandbox_contracts;
pub mod sandbox_platform;
pub mod sandbox;
pub mod tool_receipts;
pub mod tool_metadata;
pub mod tools;
pub mod worker;
pub mod wiring_context;

// Todo 45 owns these module bodies; declaring the namespaces here keeps lib.rs
// ownership with todo 43 without adding nonfunctional command implementations.
pub mod commands {}
pub mod palace {}
pub mod bindings {}
