//! Port of senpi packages/coding-agent/src/core/memory-report/kernel-registry-read.ts.
//!
//! The codemode extension's wire contract with its host process (senpi#2561, senpi#1960): the host never
//! imports the extension, which publishes one registry lister under a process-global slot; this module is
//! its single reader, so the RPC host and the memory report cannot drift apart on what it holds. The
//! listing is validated here and `busy` maps to `stale` at the boundary. The JS main-thread heap figure
//! (`readMainThreadHeapBytes`) has no Rust counterpart.

use std::sync::{Arc, LazyLock, RwLock};

use serde_json::Value;

/// The codemode registry's `list()`: one JSON object per live kernel.
pub type KernelRegistryLister = Arc<dyn Fn() -> Vec<Value> + Send + Sync>;

static KERNEL_REGISTRY: LazyLock<RwLock<Option<KernelRegistryLister>>> =
    LazyLock::new(|| RwLock::new(None));

/// Called by the codemode extension when its registry exists (upstream `KERNEL_REGISTRY_KEY`).
pub fn publish_kernel_registry(lister: KernelRegistryLister) {
    *KERNEL_REGISTRY
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(lister);
}

/// One kernel's memory as the host reports it: every consumer renders this same shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostKernelMemoryRow {
    pub session_id: String,
    pub language: String,
    pub live_bytes: u64,
    pub measure: String,
}

/// The registry's full listing; it never fabricates `live_bytes` (`last_live_bytes` is the wire field).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostKernelListing {
    pub id: String,
    pub session_id: String,
    pub language: String,
    pub measure: String,
    pub last_live_bytes: Option<u64>,
    /// A cell was running: `last_live_bytes` is the last reading, not what the kernel holds now.
    pub stale: bool,
    pub pid: Option<u64>,
}

fn string(entry: &Value, key: &str) -> Option<String> {
    entry.get(key)?.as_str().map(str::to_owned)
}

fn optional_number(entry: &Value, key: &str) -> Option<Option<u64>> {
    match entry.get(key) {
        None => Some(None),
        Some(value) => value
            .as_u64()
            .map(Some)
            .or_else(|| value.as_f64().map(|n| Some(n as u64))),
    }
}

fn parse_listing(entry: &Value) -> Option<HostKernelListing> {
    Some(HostKernelListing {
        id: string(entry, "id")?,
        session_id: string(entry, "sessionId")?,
        language: string(entry, "language")?,
        measure: string(entry, "measure")?,
        last_live_bytes: optional_number(entry, "lastLiveBytes")?,
        stale: entry.get("busy")?.as_bool()?,
        pid: optional_number(entry, "pid")?,
    })
}

/// The validated listing; entries that do not match the schema are dropped.
pub fn read_codemode_kernel_listings() -> Vec<HostKernelListing> {
    let lister = KERNEL_REGISTRY
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    lister
        .map(|list| list().iter().filter_map(parse_listing).collect())
        .unwrap_or_default()
}

/// One row per live kernel for the RPC surface; a kernel between readings lists `live_bytes: 0`.
pub fn read_codemode_kernel_rows() -> Vec<HostKernelMemoryRow> {
    read_codemode_kernel_listings()
        .into_iter()
        .map(|listing| HostKernelMemoryRow {
            session_id: listing.session_id,
            language: listing.language,
            live_bytes: listing.last_live_bytes.unwrap_or(0),
            measure: listing.measure,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn validates_the_published_listing_and_maps_busy_to_stale() {
        publish_kernel_registry(Arc::new(|| {
            vec![
                json!({"id": "k1", "sessionId": "s", "language": "js", "measure": "heap", "busy": true, "lastLiveBytes": 10}),
                json!({"id": "k2", "sessionId": "s", "language": "py", "measure": "rss", "busy": false}),
                json!({"id": "bad", "sessionId": "s", "language": "js", "measure": "heap"}),
            ]
        }));
        let listings = read_codemode_kernel_listings();
        assert_eq!(listings.len(), 2);
        assert!(listings[0].stale);
        let rows = read_codemode_kernel_rows();
        assert_eq!(rows[0].live_bytes, 10);
        assert_eq!(rows[1].live_bytes, 0);
    }
}
