//! Closed persisted vocabulary understood by rollback target R0.
pub const R0_COST_STATUSES: &[&str] = &["reported", "unavailable", "invalid"];
pub const R0_SUSPENSION_REASONS: &[&str] = &["daemon_unavailable", "host_draining"];
pub const R0_FAILURE_KINDS: &[&str] = &[
    "child-prompt-failed",
    "child-turn-failed",
    "session-create-failed",
    "depth-exceeded",
    "model_unavailable",
    "tools_unavailable",
    "session_unavailable",
    "host_unavailable",
];
pub const R0_FAILURE_REASONS: &[&str] = &[
    "model_not_in_child_profile",
    "catalog_probe_timed_out",
    "catalog_probe_failed",
    "protocol",
    "capability",
    "engine_mismatch",
    "engine_refused",
    "win32",
    "runtime",
    "host_unreachable",
    "ensure_failed",
    "ensure_timed_out",
    "open_timed_out",
    "session_path_in_use",
    "session_reservation_limit",
    "invalid_path",
    "open_failed",
    "invalid_session_context",
    "invalid_session_kind",
    "invalid_launch_profile",
    "invalid_session_id",
    "session_id_in_use",
    "host_memory_pressure",
    "host_draining",
];
