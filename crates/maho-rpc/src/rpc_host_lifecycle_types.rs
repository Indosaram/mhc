//! The HOST-level lifecycle records (senpi `rpc/rpc-host-lifecycle-types.ts`): sent to every connection,
//! about the process rather than a session. The complete wire shapes; `rpc_types::RpcHostLifecycleEvent`
//! is the decoding subset clients consume.
pub use crate::rpc_types::{RpcHostSuccessor, RpcSubscriptionLimitsEvent};
use serde::{Deserialize, Serialize};

/// Sent once to every connection before this generation starts parking for a handoff.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcHostSupersededEvent {
    pub instance_id: String,
    pub generation: f64,
    /// Public endpoint of the successor, or null for a drain without a known successor.
    pub successor: Option<RpcHostSuccessor>,
}

/// The host's event loop was blocked long enough to stall every session it serves. Informational.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcHostStalledEvent {
    /// How late the host's own 200ms timer was invoked, i.e. how long the loop was held.
    pub drift_ms: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    /// Process CPU time spent during the stalled window: near `drift_ms` the host was busy, near zero it
    /// did not run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_cpu_ms: Option<f64>,
    /// Heap change across the stalled window, in megabytes; a large drop means a collection ran.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub heap_delta_mb: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcHostMainHeap {
    pub heap_bytes: f64,
}

/// One kernel's memory as the host reports it on the pressure event and the session listing.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcHostKernelMemory {
    pub session_id: String,
    pub language: String,
    pub live_bytes: f64,
    /// `"heap"` for a JS worker's own estimate; `"footprint"` for an interpreter process.
    pub measure: String,
}

/// Emitted while the host's memory footprint is above its warning threshold. Capacity is memory, never a
/// refusal: the host reports the pressure and parks idle sessions sooner.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcHostMemoryPressureEvent {
    pub rss_mb: f64,
    /// The number compared with the threshold (senpi#2261); hosts released before it omit this and `measure`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub footprint_mb: Option<f64>,
    /// Kernel counter behind `footprint_mb`; `"rss"` when the platform exposes no footprint counter.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub measure: Option<String>,
    pub sessions: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub main: Option<RpcHostMainHeap>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kernels: Option<Vec<RpcHostKernelMemory>>,
}

/// Emitted once when a host collects. `collected` is false where the runtime exposes no full collection;
/// the record is still sent.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RpcHostTrimmedEvent {
    pub footprint_before_mb: f64,
    pub footprint_after_mb: f64,
    pub measure: String,
    pub collected: bool,
    /// Sessions still open when it collected; 0 for the zero-session trim.
    pub sessions: f64,
    pub pause_ms: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RpcHostLifecycleRecord {
    SubscriptionLimits(RpcSubscriptionLimitsEvent),
    HostSuperseded(RpcHostSupersededEvent),
    HostStalled(RpcHostStalledEvent),
    HostMemoryPressure(RpcHostMemoryPressureEvent),
    HostTrimmed(RpcHostTrimmedEvent),
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn records_round_trip_with_their_type_tag() {
        let records = [
            json!({"type":"host_superseded","instanceId":"i","generation":2.0,"successor":{"socket":"/s"}}),
            json!({"type":"host_superseded","instanceId":"i","generation":2.0,"successor":null}),
            json!({"type":"host_stalled","driftMs":900.0,"sessionId":"s","processCpuMs":850.0,"heapDeltaMb":-12.0}),
            json!({"type":"host_memory_pressure","rssMb":900.0,"footprintMb":800.0,"measure":"phys_footprint","sessions":3.0,"main":{"heapBytes":10.0},"kernels":[{"sessionId":"s","language":"python","liveBytes":0.0,"measure":"footprint"}]}),
            json!({"type":"host_trimmed","footprintBeforeMb":10.0,"footprintAfterMb":5.0,"measure":"rss","collected":false,"sessions":0.0,"pauseMs":3.0}),
        ];
        for record in records {
            let parsed: RpcHostLifecycleRecord =
                serde_json::from_value(record.clone()).expect("parse");
            assert_eq!(serde_json::to_value(parsed).expect("wire"), record);
        }
    }

    #[test]
    fn a_pre_footprint_pressure_record_still_parses() {
        let parsed: RpcHostLifecycleRecord = serde_json::from_value(
            json!({"type":"host_memory_pressure","rssMb":1.0,"sessions":0.0}),
        )
        .expect("parse");
        assert!(
            matches!(parsed, RpcHostLifecycleRecord::HostMemoryPressure(event) if event.footprint_mb.is_none())
        );
    }
}
