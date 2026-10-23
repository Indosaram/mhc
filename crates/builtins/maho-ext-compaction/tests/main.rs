//! Single integration-test binary for this crate: each file under tests/ is one module, so
//! the crate links one test executable instead of one per file.

#[path = "checkpoint_state.rs"]
mod checkpoint_state;
#[path = "circuit_breaker.rs"]
mod circuit_breaker;
#[path = "context_pipeline.rs"]
mod context_pipeline;
#[path = "context_reduction.rs"]
mod context_reduction;
#[path = "degradation_monitor.rs"]
mod degradation_monitor;
#[path = "deterministic_fallback.rs"]
mod deterministic_fallback;
#[path = "emergency_prune.rs"]
mod emergency_prune;
#[path = "extension_wiring.rs"]
mod extension_wiring;
#[path = "fallback_failed_turn_normalization.rs"]
mod fallback_failed_turn_normalization;
#[path = "faux_session.rs"]
mod faux_session;
#[path = "idle.rs"]
mod idle;
#[path = "lane_policy.rs"]
mod lane_policy;
#[path = "log.rs"]
mod log;
#[path = "message_safety.rs"]
mod message_safety;
#[path = "model_selection.rs"]
mod model_selection;
#[path = "model_usability_budget.rs"]
mod model_usability_budget;
#[path = "openai_remote.rs"]
mod openai_remote;
#[path = "openai_remote_convert.rs"]
mod openai_remote_convert;
#[path = "openai_remote_model.rs"]
mod openai_remote_model;
#[path = "openai_remote_responses_v2.rs"]
mod openai_remote_responses_v2;
#[path = "openai_remote_schema.rs"]
mod openai_remote_schema;
#[path = "openai_remote_timeout.rs"]
mod openai_remote_timeout;
#[path = "orchestration.rs"]
mod orchestration;
#[path = "overflow_retry.rs"]
mod overflow_retry;
#[path = "policy.rs"]
mod policy;
#[path = "prompts.rs"]
mod prompts;
#[path = "registration.rs"]
mod registration;
#[path = "resolved_settings.rs"]
mod resolved_settings;
#[path = "restoration_tracker.rs"]
mod restoration_tracker;
#[path = "retained_message_safety.rs"]
mod retained_message_safety;
#[path = "retry.rs"]
mod retry;
#[path = "speculative_job.rs"]
mod speculative_job;
#[path = "speculative_summary.rs"]
mod speculative_summary;
#[path = "structural_yield.rs"]
mod structural_yield;
#[path = "task_intent.rs"]
mod task_intent;
#[path = "todo_bridge.rs"]
mod todo_bridge;
#[path = "token_budget_reminder.rs"]
mod token_budget_reminder;
#[path = "tool_admission.rs"]
mod tool_admission;
#[path = "tool_truncation.rs"]
mod tool_truncation;
#[path = "transient_failure.rs"]
mod transient_failure;
