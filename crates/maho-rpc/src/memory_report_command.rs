//! Port of senpi packages/coding-agent/src/modes/rpc/memory-report-command.ts.
use maho_core::memory_report::{
    memory_report_registry::memory_report_session_for,
    memory_report_write::{MemoryReportOutcome, write_memory_reports},
};

/// `memory_report`: writes the session's report on demand; refused unless the host runs with
/// SENPI_MEMORY_REPORT=1 (the session registered no source without it).
pub async fn answer_memory_report(owner: u64) -> Result<serde_json::Value, String> {
    let Some(source) = memory_report_session_for(owner) else {
        return Err("memory_report_disabled: start the host with SENPI_MEMORY_REPORT=1".into());
    };
    match write_memory_reports(&[source]).await.into_iter().next() {
        Some(MemoryReportOutcome::Written {
            path,
            heap_snapshot,
        }) => {
            let mut data = serde_json::json!({ "path": path });
            if let Some(snapshot) = heap_snapshot {
                data["heapSnapshot"] = snapshot.into();
            }
            Ok(data)
        }
        Some(MemoryReportOutcome::Failed { error }) => {
            Err(format!("memory_report_failed: {error}"))
        }
        None => Err("memory_report_failed: no report written".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn an_unregistered_session_is_refused_as_disabled() {
        assert_eq!(
            answer_memory_report(u64::MAX).await,
            Err("memory_report_disabled: start the host with SENPI_MEMORY_REPORT=1".into())
        );
    }

    #[tokio::test]
    async fn a_registered_session_answers_the_written_path() {
        struct Source(String);
        impl maho_core::memory_report::memory_report_registry::MemoryReportSessionSource for Source {
            fn session_id(&self) -> String {
                "s".into()
            }
            fn session_file(&self) -> Option<String> {
                Some(self.0.clone())
            }
            fn resident_store(&self) -> maho_core::session_resident_store_size::ResidentStoreSize {
                Default::default()
            }
            fn reporters(&self) -> Vec<maho_ext_api::NamedMemoryReporter> {
                Vec::new()
            }
        }
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("a.jsonl").to_string_lossy().into_owned();
        let env = std::collections::HashMap::from([(
            maho_core::memory_report::memory_report_registry::MEMORY_REPORT_ENV.to_owned(),
            "1".to_owned(),
        )]);
        let unregister =
            maho_core::memory_report::memory_report_registry::register_memory_report_session(
                u64::MAX - 1,
                std::sync::Arc::new(Source(file)),
                &env,
            );
        let data = answer_memory_report(u64::MAX - 1).await.unwrap();
        unregister();
        let path = data["path"].as_str().unwrap();
        assert!(std::path::Path::new(path).is_file());
        assert!(data.get("heapSnapshot").is_none());
    }
}
