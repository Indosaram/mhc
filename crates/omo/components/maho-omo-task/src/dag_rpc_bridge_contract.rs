use std::sync::Arc;
use serde_json::Value;
use senpi_task::{dag::types::DagRunSnapshot, manager::Unsubscribe};
pub type DagBridgeListener = Arc<dyn Fn(&Value) + Send + Sync>;
pub type DagBridgeSubscribe = Arc<dyn Fn(DagBridgeListener) -> Unsubscribe + Send + Sync>;
pub type DagBridgeEmit = Arc<dyn Fn(&str, Value) + Send + Sync>;
pub type DagBridgeSnapshots = Arc<dyn Fn() -> Vec<(DagRunSnapshot, String)> + Send + Sync>;
pub struct DagBridgeRun { pub run_id: String, pub status: String, pub subscribe: DagBridgeSubscribe }
pub struct DagRpcBridgeDeps {
    pub live_runs: Arc<dyn Fn() -> Vec<DagBridgeRun> + Send + Sync>,
    pub run_snapshots: Option<DagBridgeSnapshots>,
    pub parent_session_id: Arc<dyn Fn() -> Option<String> + Send + Sync>,
    pub emit: DagBridgeEmit,
    pub timers: Arc<dyn crate::status_ui::StatusUiTimers>,
    pub now: Arc<dyn Fn() -> i64 + Send + Sync>,
    pub heartbeat_ms: Option<u64>,
    pub activity_coalesce_ms: Option<u64>,
    pub snapshot_debounce_ms: Option<u64>,
}
