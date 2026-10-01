//! Compaction policy primitives ported from senpi's builtin compaction extension.
pub mod policy;
pub mod speculation_lead;
pub mod idle;
pub mod idle_retry;
pub mod summarization_retry;
pub mod task_intent;
pub mod token_budget_reminder;
pub mod tool_truncation;
pub mod state;
pub mod circuit_breaker;
pub mod per_turn_cap;
pub mod degradation_monitor;
pub mod repair_tool_pairs;
pub mod summarization_turn_order;
pub mod overflow_retry;
pub mod tool_admission;
pub mod context_reduction;
