//! Forward definitions of types owned by modules of other port slices (dag, manager, tools).
//!
//! The TypeScript modules import each other cyclically; slice A needs these shapes before the
//! owning modules are ported. Each type mirrors its TypeScript definition exactly and is listed in
//! `parity.md` so the owning slice can move it without changing its shape.

use serde::Serialize;
use serde_json::Value;

/// `dag/owner.ts` `DagTaskOwner`: the DAG node that owns a task record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DagTaskOwner {
    /// Always `"dag"` on the wire.
    pub kind: DagOwnerKind,
    #[serde(rename = "runId")]
    pub run_id: String,
    #[serde(rename = "nodeId")]
    pub node_id: String,
    pub fingerprint: String,
}

/// `dag/owner.ts` `DagTaskOwnerKey`: the owner identity without its fingerprint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DagTaskOwnerKey {
    pub run_id: String,
    pub node_id: String,
}

impl DagTaskOwner {
    pub fn key(&self) -> DagTaskOwnerKey {
        DagTaskOwnerKey {
            run_id: self.run_id.clone(),
            node_id: self.node_id.clone(),
        }
    }

    pub fn matches(&self, key: &DagTaskOwnerKey) -> bool {
        self.run_id == key.run_id && self.node_id == key.node_id
    }
}

/// The only owner kind the TypeScript source defines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DagOwnerKind {
    Dag,
}

/// `manager/child-handle.ts` `ManagedChildEvent`: one event from a managed child's stream.
/// Payload fields stay untyped JSON because they cross the host runtime boundary.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ManagedChildEvent {
    /// The TypeScript `type` discriminator (`message_end`, `tool_execution_start`, ...).
    pub event_type: String,
    pub message: Option<Value>,
    pub tool_call_id: Option<String>,
    pub tool_name: Option<String>,
    pub args: Option<Value>,
    pub input: Option<Value>,
    pub result: Option<Value>,
    pub is_error: Option<bool>,
    pub to: Option<String>,
    /// `retry_fallback_*` telemetry strings (`from`, `chainKey`, `reason`, `lastError`).
    pub from: Option<String>,
    pub chain_key: Option<String>,
    pub reason: Option<String>,
    pub last_error: Option<String>,
}

impl ManagedChildEvent {
    /// An event carrying only its type.
    pub fn of(event_type: &str) -> Self {
        Self {
            event_type: event_type.to_string(),
            ..Self::default()
        }
    }
}

/// `tools/run-stats-format.ts` `formatRunDuration` (owned by the tools slice): `2h 5m`, `3m 4s`, `7s`.
pub fn format_run_duration(duration_ms: i64) -> String {
    let total_seconds = (duration_ms.max(0) + 500) / 1_000;
    let hours = total_seconds / 3_600;
    let minutes = (total_seconds % 3_600) / 60;
    let seconds = total_seconds % 60;
    if hours > 0 {
        format!("{hours}h {minutes}m")
    } else if minutes > 0 {
        format!("{minutes}m {seconds}s")
    } else {
        format!("{seconds}s")
    }
}

/// JS `String.prototype.slice(0, limit)` over UTF-16 code units, never splitting a surrogate pair.
pub(crate) fn utf16_prefix(value: &str, limit: usize) -> &str {
    let mut units = 0;
    for (index, ch) in value.char_indices() {
        units += ch.len_utf16();
        if units > limit {
            return &value[..index];
        }
    }
    value
}

/// `new Date(ms).toISOString()`: millisecond UTC timestamps with a `Z` suffix.
pub(crate) fn iso_from_ms(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .unwrap_or_default()
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string()
}
