//! Pool records, items, workers and the error type (spec §1, §3).
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// Machine error codes (spec §1, interface facts).
pub mod codes {
    pub const INVALID_INPUT: &str = "invalid_input";
    pub const INVALID_POOL_ID: &str = "invalid_pool_id";
    pub const POOL_NOT_FOUND: &str = "pool_not_found";
    pub const SCOPE_DENIED: &str = "scope_denied";
    pub const POOL_NAME_CONFLICT: &str = "pool_name_conflict";
    pub const POOL_CLOSED: &str = "pool_closed";
    pub const DUPLICATE_KEY_CONFLICT: &str = "duplicate_key_conflict";
    pub const TOOLS_UNAVAILABLE: &str = "tools_unavailable";
    pub const ADMISSION_REFUSED: &str = "admission_refused";
    pub const SPAWN_FAILED: &str = "spawn_failed";
    pub const POLICY_DENIED: &str = "policy_denied";
    pub const WORKER_NOT_CONTINUABLE: &str = "worker_not_continuable";
    pub const WORKER_UNASSIGNED: &str = "worker_unassigned";
    pub const STALE_ASSIGNMENT: &str = "stale_assignment";
    pub const STORE_CORRUPT: &str = "store_corrupt";
    pub const CANCELLED: &str = "cancelled";
    pub const YIELD_CONFLICT: &str = "yield_conflict";
    pub const ITEM_MISSING_YIELD: &str = "item_missing_yield";
    pub const DELIVERY_UNCERTAIN: &str = "delivery_uncertain";
    pub const TOOL_NAME_COLLISION: &str = "tool_name_collision";
    pub const KERNEL_TOOL_MISSING: &str = "kernel_tool_missing";
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PoolMode {
    Fresh,
    #[default]
    KeepAlive,
}

impl PoolMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fresh => "fresh",
            Self::KeepAlive => "keep_alive",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PoolStatus {
    Open,
    Closing,
    Cancelled,
}

impl PoolStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Closing => "closing",
            Self::Cancelled => "cancelled",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemStatus {
    Queued,
    Assigned,
    Completed,
    Error,
    Cancelled,
}

impl ItemStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Assigned => "assigned",
            Self::Completed => "completed",
            Self::Error => "error",
            Self::Cancelled => "cancelled",
        }
    }
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Error | Self::Cancelled)
    }
}

/// The worker agent of a pool: `{category, prompt, model?}` or `{subagent_type, prompt, model?}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AgentSpec {
    Category {
        category: String,
        prompt: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
    },
    Subagent {
        subagent_type: String,
        prompt: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
    },
}

impl AgentSpec {
    pub fn prompt(&self) -> &str {
        match self {
            Self::Category { prompt, .. } | Self::Subagent { prompt, .. } => prompt,
        }
    }
    pub fn model(&self) -> Option<&str> {
        match self {
            Self::Category { model, .. } | Self::Subagent { model, .. } => model.as_deref(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ItemError {
    pub code: String,
    pub message: String,
}

impl ItemError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self { code: code.to_owned(), message: message.into() }
    }
}

/// Which worker run an item was sent to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Binding {
    /// Empty while the start of a new worker is in flight (the assignment intent is persisted
    /// before the host is asked; a restart that finds it empty marks the item
    /// `delivery_uncertain`).
    pub task_id: String,
    pub run_epoch: u64,
    pub generation: u64,
    /// The host accepted the assignment (start or follow-up returned successfully).
    #[serde(default)]
    pub delivered: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PoolItem {
    pub key: String,
    pub input: Value,
    pub item_id: String,
    pub status: ItemStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ItemError>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binding: Option<Binding>,
}

impl PoolItem {
    pub fn fail(&mut self, code: &str, message: impl Into<String>) {
        self.status = ItemStatus::Error;
        self.data = None;
        self.error = Some(ItemError::new(code, message));
    }

    /// `{key, data}` or `{key, error}` (aggregate entry, spec §7).
    pub fn result_json(&self) -> Value {
        match (&self.status, &self.error) {
            (ItemStatus::Completed, _) => {
                json!({"key": self.key, "data": self.data.clone().unwrap_or(Value::Null)})
            }
            (_, Some(error)) => {
                json!({"key": self.key, "error": {"code": error.code, "message": error.message}})
            }
            (status, None) => json!({"key": self.key, "error": {
                "code": codes::DELIVERY_UNCERTAIN,
                "message": format!("item ended {} without a result", status.as_str())
            }}),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerStatus {
    Busy,
    Idle,
    Paused,
}

impl WorkerStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Busy => "busy",
            Self::Idle => "idle",
            Self::Paused => "paused",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PoolWorker {
    pub task_id: String,
    pub run_epoch: u64,
    pub status: WorkerStatus,
    pub completed_turns: u64,
    /// Milliseconds since the Unix epoch when the worker last became idle.
    pub idle_since: u64,
    /// The worker's own session id (used by its process to find this pool).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Never selected again (fresh-mode worker after its run, or a worker that could not be
    /// continued).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub retired: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AggregateState {
    pub generation: u64,
    pub delivered: bool,
    pub accepted: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PoolRecord {
    pub pool_id: String,
    pub parent_session: String,
    pub name: String,
    pub agent: AgentSpec,
    pub mode: PoolMode,
    pub status: PoolStatus,
    pub generation: u64,
    pub revision: u64,
    pub items: Vec<PoolItem>,
    pub workers: Vec<PoolWorker>,
    pub aggregate: Option<AggregateState>,
    /// Extra worker tools, sorted and deduplicated.
    #[serde(default)]
    pub tools: Vec<String>,
    /// The resolved worker spec (compared by idempotent `create`).
    #[serde(default)]
    pub resolved: Value,
}

impl PoolRecord {
    pub fn worker(&self, task_id: &str) -> Option<&PoolWorker> {
        self.workers.iter().find(|w| w.task_id == task_id)
    }
    pub fn worker_mut(&mut self, task_id: &str) -> Option<&mut PoolWorker> {
        self.workers.iter_mut().find(|w| w.task_id == task_id)
    }
    pub fn all_terminal(&self) -> bool {
        self.items.iter().all(|i| i.status.is_terminal())
    }

    /// The `inspect` response (spec §1).
    pub fn inspect_json(&self) -> Value {
        let items: Vec<Value> = self
            .items
            .iter()
            .map(|item| {
                let mut out = json!({
                    "key": item.key,
                    "input": item.input,
                    "item_id": item.item_id,
                    "status": item.status.as_str(),
                });
                match item.status {
                    ItemStatus::Completed => {
                        out["data"] = item.data.clone().unwrap_or(Value::Null);
                    }
                    ItemStatus::Error | ItemStatus::Cancelled => {
                        if let Some(error) = &item.error {
                            out["error"] = json!({"code": error.code, "message": error.message});
                        }
                    }
                    ItemStatus::Queued | ItemStatus::Assigned => {}
                }
                out
            })
            .collect();
        let workers: Vec<Value> = self
            .workers
            .iter()
            .map(|w| {
                json!({
                    "task_id": w.task_id,
                    "run_epoch": w.run_epoch,
                    "status": w.status.as_str(),
                    "completed_turns": w.completed_turns,
                    "idle_since": w.idle_since,
                })
            })
            .collect();
        json!({
            "pool_id": self.pool_id,
            "name": self.name,
            "mode": self.mode.as_str(),
            "status": self.status.as_str(),
            "generation": self.generation,
            "revision": self.revision,
            "items": items,
            "workers": workers,
            "aggregate": self.aggregate.as_ref().map(|a| json!({
                "generation": a.generation, "delivered": a.delivered, "accepted": a.accepted
            })),
        })
    }
}

/// A failure with its machine code; displayed as `code: message`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PoolError {
    pub code: &'static str,
    pub message: String,
}

impl PoolError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self { code, message: message.into() }
    }
}

impl std::fmt::Display for PoolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for PoolError {}

/// `wp_` + 32 lowercase hex digits.
pub fn new_pool_id() -> String {
    format!("wp_{}", uuid::Uuid::new_v4().simple())
}

/// `wi_` + 32 lowercase hex digits.
pub fn new_item_id() -> String {
    format!("wi_{}", uuid::Uuid::new_v4().simple())
}

fn is_hex_id(id: &str, prefix: &str) -> bool {
    id.strip_prefix(prefix).is_some_and(|rest| {
        rest.len() == 32 && rest.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

/// Store-boundary check of a pool id (`invalid_pool_id`).
///
/// # Errors
/// The id is not `wp_` + 32 lowercase hex digits.
pub fn validate_pool_id(id: &str) -> Result<(), PoolError> {
    if is_hex_id(id, "wp_") {
        Ok(())
    } else {
        Err(PoolError::new(codes::INVALID_POOL_ID, format!("malformed pool id {id:?}")))
    }
}

pub fn is_item_id(id: &str) -> bool {
    is_hex_id(id, "wi_")
}

/// JSON text with object keys sorted at every level (canonical equality, spec §3).
pub fn canonical(value: &Value) -> String {
    fn sort(value: &Value) -> Value {
        match value {
            Value::Object(map) => {
                let sorted: BTreeMap<&String, Value> = map.iter().map(|(k, v)| (k, sort(v))).collect();
                Value::Object(sorted.into_iter().map(|(k, v)| (k.clone(), v)).collect())
            }
            Value::Array(items) => Value::Array(items.iter().map(sort).collect()),
            other => other.clone(),
        }
    }
    serde_json::to_string(&sort(value)).unwrap_or_default()
}

/// Milliseconds since the Unix epoch.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}
