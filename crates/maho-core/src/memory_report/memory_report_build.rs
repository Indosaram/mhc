//! Port of senpi packages/coding-agent/src/core/memory-report/memory-report-build.ts.
//!
//! NATIVE-DIVERGENCE(senpi `mainThreadMemory`): a native process has no JS heap; `main` reports the process
//! footprint only (`heapUsed`/`external` are the JS engine's and have no native counterpart, so they are 0), and the
//! TUI frame-line figure is never published by a native renderer.
use serde_json::{Map, Value, json};

use super::kernel_registry_read::read_codemode_kernel_listings;
use super::memory_report_registry::{
    MemoryReportSessionSource, RESERVED_MEMORY_REPORT_KEYS, tui_render_cache_totals,
};

/// One report: the core sections plus one section per extension reporter, keyed by the reporter's name.
pub fn build_memory_report(
    source: &dyn MemoryReportSessionSource,
    heap_snapshot: Option<&str>,
    taken_at: &str,
) -> Value {
    let (sections, errors) = reporter_sections(&source.reporters());
    let mut report = sections;
    let footprint = crate::process_footprint::read_own_footprint();
    let resident = source.resident_store();
    report.insert("sessionId".into(), source.session_id().into());
    report.insert("takenAt".into(), taken_at.into());
    report.insert("pid".into(), std::process::id().into());
    report.insert(
        "main".into(),
        json!({"heapUsed":0,"external":0,"footprint":{"bytes":footprint.bytes,"measure":footprint.measure.as_str()}}),
    );
    report.insert(
        "kernels".into(),
        Value::Array(
            read_codemode_kernel_listings()
                .into_iter()
                .map(|kernel| {
                    let mut row = json!({"id":kernel.id,"sessionId":kernel.session_id,"language":kernel.language,"measure":kernel.measure,"stale":kernel.stale});
                    if let Some(bytes) = kernel.last_live_bytes {
                        row["lastLiveBytes"] = bytes.into();
                    }
                    if let Some(pid) = kernel.pid {
                        row["pid"] = pid.into();
                    }
                    row
                })
                .collect(),
        ),
    );
    report.insert(
        "residentStore".into(),
        json!({"entries":resident.entries,"approxBytes":resident.approx_bytes}),
    );
    if let Some(totals) = tui_render_cache_totals() {
        report.insert(
            "tuiRenderCache".into(),
            json!({"components":totals.components,"cachedLines":totals.cached_lines,"images":totals.images,"finishedCards":totals.finished_cards,"cachedLinesBytes":totals.cached_lines_bytes,"resultBytes":totals.result_bytes}),
        );
    }
    if let Some(snapshot) = heap_snapshot {
        report.insert("heapSnapshot".into(), snapshot.into());
    }
    if !errors.is_empty() {
        report.insert("reporterErrors".into(), Value::Object(errors));
    }
    Value::Object(report)
}

/// One section per reporter, skipping reserved and repeated names; a reporter that panics is named in the errors.
fn reporter_sections(
    reporters: &[maho_ext_api::NamedMemoryReporter],
) -> (Map<String, Value>, Map<String, Value>) {
    let mut sections = Map::new();
    let mut errors = Map::new();
    for named in reporters {
        let name = named.name.as_str();
        if RESERVED_MEMORY_REPORT_KEYS.contains(&name)
            || sections.contains_key(name)
            || errors.contains_key(name)
        {
            continue;
        }
        let reporter = named.reporter.clone();
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || reporter())) {
            Ok(figures) => {
                sections.insert(
                    name.to_owned(),
                    Value::Object(
                        figures
                            .into_iter()
                            .map(|(key, value)| (key, value.into()))
                            .collect(),
                    ),
                );
            }
            Err(panic) => {
                let message = panic
                    .downcast_ref::<&str>()
                    .map(|text| (*text).to_owned())
                    .or_else(|| panic.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "reporter failed".to_owned());
                errors.insert(name.to_owned(), message.into());
            }
        }
    }
    (sections, errors)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_resident_store_size::ResidentStoreSize;
    use std::sync::Arc;

    struct Source(Vec<maho_ext_api::NamedMemoryReporter>);
    impl MemoryReportSessionSource for Source {
        fn session_id(&self) -> String {
            "s1".into()
        }
        fn session_file(&self) -> Option<String> {
            None
        }
        fn resident_store(&self) -> ResidentStoreSize {
            ResidentStoreSize {
                entries: 2,
                approx_bytes: 10,
            }
        }
        fn reporters(&self) -> Vec<maho_ext_api::NamedMemoryReporter> {
            self.0.clone()
        }
    }

    /// senpi memory-report-build.ts: reporter sections never shadow a core key, the first of a repeated name wins,
    /// and the core sections carry the session's identity and resident store.
    #[test]
    fn reporter_sections_never_shadow_core_keys() {
        let reporter = |name: &str, figure: usize| maho_ext_api::NamedMemoryReporter {
            name: name.into(),
            reporter: Arc::new(move || [("n".to_owned(), figure)].into_iter().collect()),
        };
        let report = build_memory_report(
            &Source(vec![
                reporter("cache", 1),
                reporter("cache", 2),
                reporter("pid", 3),
            ]),
            None,
            "t",
        );
        assert_eq!(report["cache"], json!({"n":1}));
        assert_eq!(report["pid"], std::process::id());
        assert_eq!(report["sessionId"], "s1");
        assert_eq!(report["takenAt"], "t");
        assert_eq!(
            report["residentStore"],
            json!({"entries":2,"approxBytes":10})
        );
        assert!(report.get("reporterErrors").is_none());
    }
}
