pub const DELEGATION_STATUSES: &[&str] = &["completed", "error", "cancelled", "interrupted", "lost"];
pub const DELEGATION_START_REASONS: &[&str] = &[
  "initial_spawn",
  "runtime_fallback",
  "session_resume",
  "dag_retry",
  "revive_after_completed",
  "revive_after_error",
  "revive_after_cancelled",
  "revive_after_interrupted",
  "revive_after_lost",
  "unknown",
];
pub const DELEGATION_OWNER_KINDS: &[&str] = &["plain_child", "dag_node", "team_member", "unknown"];
pub const DELEGATION_BACKGROUND_MODES: &[&str] = &["foreground", "background", "promoted", "unknown"];
pub const DELEGATION_REASONING_EFFORTS: &[&str] = &[
  "off", "minimal", "low", "medium", "high", "xhigh", "max", "other", "none",
];
pub const DELEGATION_MODEL_SOURCES: &[&str] = &["category", "explicit", "agent", "none"];
pub const DELEGATION_COVERAGE_STATUSES: &[&str] = &["complete", "partial", "unavailable"];
pub const DELEGATION_COST_STATUSES: &[&str] = &["reported", "partial", "unavailable", "invalid"];
pub const DELEGATION_DURATION_STATUSES: &[&str] = &["monotonic", "wall_clock", "unavailable"];
use serde_json::{Value, json};
pub fn build_delegation_completed_schema(providers: &[&str], models: &[&str]) -> Value {
    let agents: Vec<_> = crate::product_identity::curated_agents().into_iter().chain(["custom", "none"]).collect();
    let categories: Vec<_> = crate::product_identity::builtin_category_names().into_iter().chain(["custom", "none"]).collect();
    json!({
        "$session_id": {"type":"string"},
        "agent_type": {"type":"string","values":agents},
        "category": {"type":"string","values":categories},
        "execution_mode": {"type":"string","values":["in-process","process"]},
        "model_id": {"type":"string","values":models},
        "provider": {"type":"string","values":providers},
        "background_mode": {"type":"string","values":DELEGATION_BACKGROUND_MODES},
        "cache_read_tokens": {"type":"number"},
        "cache_write_tokens": {"type":"number"},
        "config_generation": {"type":"number"},
        "cost_status": {"type":"string","values":DELEGATION_COST_STATUSES},
        "cost_usd": {"type":"number"},
        "duration_ms": {"type":"number"},
        "duration_status": {"type":"string","values":DELEGATION_DURATION_STATUSES},
        "fallback_attempts": {"type":"number"},
        "input_tokens": {"type":"number"},
        "model_source": {"type":"string","values":DELEGATION_MODEL_SOURCES},
        "output_tokens": {"type":"number"},
        "owner_kind": {"type":"string","values":DELEGATION_OWNER_KINDS},
        "reasoning_effort": {"type":"string","values":DELEGATION_REASONING_EFFORTS},
        "run_epoch": {"type":"number"},
        "start_reason": {"type":"string","values":DELEGATION_START_REASONS},
        "stats_status": {"type":"string","values":DELEGATION_COVERAGE_STATUSES},
        "status": {"type":"string","values":DELEGATION_STATUSES},
        "task_send_queued_count": {"type":"number"},
        "task_send_running_count": {"type":"number"},
        "task_seq": {"type":"number"},
        "token_status": {"type":"string","values":DELEGATION_COVERAGE_STATUSES},
        "tool_calls": {"type":"number"},
        "total_tokens": {"type":"number"},
        "turns": {"type":"number"}
    })
}
