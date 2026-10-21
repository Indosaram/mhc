pub const PROCESS_KINDS: &[&str] = &["interactive", "print", "json", "rpc-host", "task-child", "unknown"];
pub const CRASH_DETECTIONS: &[&str] = &["supervisor", "parent", "unclean_exit", "unknown"];
pub const CRASH_SIGNALS: &[&str] = &[
  "SIGSEGV", "SIGBUS", "SIGILL", "SIGTRAP", "SIGABRT", "SIGFPE", "SIGKILL", "SIGTERM", "SIGHUP", "SIGINT",
  "SIGQUIT", "SIGSYS", "other", "none", "unknown",
];
pub const UPTIME_BUCKETS: &[&str] = &["lt_1m", "1_10m", "10_60m", "1_6h", "6_24h", "24h_plus"];
pub const SHARD_KINDS: &[&str] = &["p", "i", "none", "unknown"];
pub static PROCESS_CRASHED_SCHEMA: std::sync::LazyLock<serde_json::Value> = std::sync::LazyLock::new(|| serde_json::json!({
    "$os": {"type":"string"},
    "arch": {"type":"string"},
    "crashed_bun_version": {"type":"string"},
    "crashed_engine_version": {"type":"string"},
    "crashed_omo_version": {"type":"string"},
    "detection": {"type":"string", "values": CRASH_DETECTIONS},
    "exit_code": {"type":"number"},
    "process_kind": {"type":"string", "values": PROCESS_KINDS},
    "shard_kind": {"type":"string", "values": SHARD_KINDS},
    "signal": {"type":"string", "values": CRASH_SIGNALS},
    "uptime_bucket": {"type":"string", "values": UPTIME_BUCKETS},
    "uptime_ms": {"type":"number"}
}));
