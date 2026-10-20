use maho_core::agent_session::AgentSession;
use serde_json::{Value, json};

#[cfg(windows)]
#[path = "memory_report_windows.rs"]
mod windows;

pub fn build_memory_report(session: &AgentSession, stamp: &str) -> Result<Value, String> {
    let resident = session.with_session_manager(|manager| manager.resident_store_stats());
    let mut report = json!({
        "sessionId":session.session_id(), "takenAt":stamp, "pid":std::process::id(),
        "kernels": kernel_listings(),
        "residentStore":{"entries":resident.blob_count,"approxBytes":resident.blob_bytes}
    });
    append_reporter_sections(&mut report, session.extension_memory_reporters());
    if let Some(totals) =
        maho_interactive::components::tool_execution_cache::tui_render_cache_totals()
    {
        report["tuiRenderCache"] = json!(totals);
    }
    if let Some(bytes) = maho_interactive::tui_renderer::previous_lines_bytes() {
        report["tui"] = json!({"previousLinesBytes": bytes});
    }
    #[cfg(target_os = "linux")]
    let report = {
        let mut report = report;
        let status =
            std::fs::read_to_string("/proc/self/status").map_err(|error| error.to_string())?;
        report["main"] = json!({"footprint": linux_footprint(&status)?});
        report
    };
    #[cfg(windows)]
    {
        report["main"] = json!({"footprint": windows::read_own_footprint()?});
    }
    #[cfg(target_os = "macos")]
    {
        let pid = i32::try_from(std::process::id()).map_err(|error| error.to_string())?;
        report["main"] = json!({"footprint": macos_footprint(
            libproc::libproc::pid_rusage::pidrusage::<libproc::libproc::pid_rusage::RUsageInfoV2>(pid)
        )?});
    }
    Ok(report)
}

#[cfg(target_os = "macos")]
fn macos_footprint(
    usage: Result<libproc::libproc::pid_rusage::RUsageInfoV2, String>,
) -> Result<Value, String> {
    let usage = usage?;
    Ok(json!({"bytes": usage.ri_phys_footprint, "measure": "phys_footprint"}))
}

fn append_reporter_sections(
    report: &mut Value,
    reporters: Vec<maho_ext_api::NamedMemoryReporter>,
) {
    let mut seen = std::collections::BTreeSet::new();
    let mut errors = serde_json::Map::new();
    for maho_ext_api::NamedMemoryReporter { name, reporter } in reporters {
        if maho_ext_api::RESERVED_MEMORY_REPORT_KEYS.contains(&name.as_str())
            || !seen.insert(name.clone())
        {
            continue;
        }
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| reporter())) {
            Ok(figures) => report[&name] = json!(figures),
            Err(error) => {
                let message = error.downcast_ref::<String>().map(String::as_str)
                    .or_else(|| error.downcast_ref::<&str>().copied())
                    .unwrap_or("Memory reporter panicked with a non-string payload");
                errors.insert(name, json!(message));
            }
        }
    }
    if !errors.is_empty() {
        report["reporterErrors"] = Value::Object(errors);
    }
}

fn kernel_listings() -> Vec<Value> {
    maho_codemode::extension::kernel_registry::KERNEL_REGISTRY
        .list()
        .into_iter()
        .map(kernel_listing)
        .collect()
}

fn kernel_listing(kernel: maho_codemode::extension::kernel_registry::KernelListing) -> Value {
    let mut row = json!({
        "id":kernel.id, "sessionId":kernel.session_id, "language":kernel.language,
        "measure":kernel.measure, "stale":kernel.busy
    });
    if let Some(bytes) = kernel.last_live_bytes {
        row["lastLiveBytes"] = json!(bytes);
    }
    if let Some(pid) = kernel.pid {
        row["pid"] = json!(pid);
    }
    row
}

#[cfg(any(target_os = "linux", test))]
fn linux_footprint(status: &str) -> Result<Value, String> {
    for (prefix, measure) in [("RssAnon:", "rss_anon"), ("VmRSS:", "rss")] {
        if let Some(bytes) = status
            .lines()
            .find_map(|line| line.strip_prefix(prefix))
            .and_then(|value| value.trim().strip_suffix(" kB"))
            .and_then(|value| value.trim().parse::<u64>().ok())
            .and_then(|kb| kb.checked_mul(1024))
        {
            return Ok(json!({"bytes":bytes, "measure":measure}));
        }
    }
    Err("Process status contains neither anonymous resident memory nor RSS".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use maho_codemode::extension::kernel_registry::{KernelListing, KernelMeasure};
    use maho_codemode::tool::types::EvalLanguage;

    #[test]
    fn reporters_keep_first_name_and_cannot_replace_core_sections() {
        // Given
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let skipped = calls.clone();
        let ignored: maho_ext_api::MemoryReporter = std::sync::Arc::new(move || {
            skipped.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Default::default()
        });
        let mut report = json!({"sessionId": "session"});
        let mut reporters = vec![
            maho_ext_api::NamedMemoryReporter {
                name: "cache".into(),
                reporter: std::sync::Arc::new(|| [("bytes".into(), 41)].into()),
            },
            maho_ext_api::NamedMemoryReporter { name: "cache".into(), reporter: ignored.clone() },
        ];
        reporters.extend(maho_ext_api::RESERVED_MEMORY_REPORT_KEYS.iter().map(|name| {
            maho_ext_api::NamedMemoryReporter { name: (*name).into(), reporter: ignored.clone() }
        }));
        // When
        append_reporter_sections(&mut report, reporters);
        // Then
        assert_eq!(report["cache"], json!({"bytes": 41}));
        assert_eq!(report["sessionId"], "session");
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert!(report.get("reporterErrors").is_none());
    }

    #[test]
    fn reporter_panic_keeps_first_failure_and_remaining_sections() {
        // Given
        let mut report = json!({});
        let reporters = vec![
            maho_ext_api::NamedMemoryReporter {
                name: "broken".into(), reporter: std::sync::Arc::new(|| panic!("reporter failed")),
            },
            maho_ext_api::NamedMemoryReporter {
                name: "broken".into(), reporter: std::sync::Arc::new(|| panic!("duplicate ran")),
            },
            maho_ext_api::NamedMemoryReporter {
                name: "healthy".into(), reporter: std::sync::Arc::new(|| [("bytes".into(), 0)].into()),
            },
        ];
        // When
        append_reporter_sections(&mut report, reporters);
        // Then
        assert!(report.get("broken").is_none());
        assert_eq!(report["reporterErrors"], json!({"broken": "reporter failed"}));
        assert_eq!(report["healthy"], json!({"bytes": 0}));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_footprint_uses_physical_bytes_not_resident_size() {
        // Given
        let usage = libproc::libproc::pid_rusage::RUsageInfoV2 {
            ri_phys_footprint: 9_007_199_254_740_993,
            ri_resident_size: 88_000,
            ..Default::default()
        };
        // When
        let footprint = macos_footprint(Ok(usage)).expect("footprint");
        // Then
        assert_eq!(footprint["bytes"].as_u64(), Some(9_007_199_254_740_993));
        assert_eq!(footprint["measure"], "phys_footprint");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_zero_footprint_does_not_fall_back_to_resident_size() {
        // Given
        let usage = libproc::libproc::pid_rusage::RUsageInfoV2 {
            ri_resident_size: 88_000,
            ..Default::default()
        };
        // When
        let footprint = macos_footprint(Ok(usage)).expect("footprint");
        // Then
        assert_eq!(footprint, json!({"bytes": 0, "measure": "phys_footprint"}));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_query_failure_propagates_without_fabricating_measurements() {
        // Given
        let usage = Err("pidrusage failed".to_owned());
        // When
        let footprint = macos_footprint(usage);
        // Then
        assert_eq!(footprint, Err("pidrusage failed".into()));
    }

    #[test]
    fn kernel_listing_preserves_unmeasured_state_and_busy_staleness() {
        // Given
        let kernel = KernelListing {
            id: "kernel".into(),
            session_id: "session".into(),
            language: EvalLanguage::Js,
            measure: KernelMeasure::Heap,
            last_live_bytes: None,
            busy: true,
            pid: None,
        };
        // When
        let row = kernel_listing(kernel);
        // Then
        assert_eq!(row["stale"], true);
        assert_eq!(row["sessionId"], "session");
        assert!(row.get("lastLiveBytes").is_none());
        assert!(row.get("liveBytes").is_none());
        assert!(row.get("busy").is_none());
        assert!(row.get("pid").is_none());
    }

    #[test]
    fn anonymous_resident_memory_takes_precedence_over_rss() {
        // Given
        let status = "VmRSS: 900 kB\nRssAnon: 12 kB\n";
        // When
        let footprint = linux_footprint(status).expect("footprint");
        // Then
        assert_eq!(footprint, json!({"bytes":12288,"measure":"rss_anon"}));
    }

    #[test]
    fn absent_anonymous_memory_falls_back_to_labelled_rss() {
        // Given
        let status = "VmRSS: 4 kB\n";
        // When
        let footprint = linux_footprint(status).expect("footprint");
        // Then
        assert_eq!(footprint, json!({"bytes":4096,"measure":"rss"}));
    }
}
