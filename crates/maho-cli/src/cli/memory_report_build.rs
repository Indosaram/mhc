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
    Ok(report)
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
