//! Port of senpi packages/coding-agent/src/core/memory-report/memory-report-write.ts.
//!
//! NATIVE-DIVERGENCE(senpi `writeSnapshot`): a native process has no V8/JSC heap to snapshot, so
//! `SENPI_MEMORY_REPORT_SNAPSHOT=1` adds nothing and no report names a `heapSnapshot`.
use std::path::PathBuf;

use super::memory_report_build::build_memory_report;
use super::memory_report_registry::MemoryReportSessionSource;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemoryReportOutcome {
    Written {
        path: String,
        heap_snapshot: Option<String>,
    },
    Failed {
        error: String,
    },
}

/// `<session>.jsonl` keeps its reports in `<session>-artifacts/memory/`, beside the eval artifacts; an unsaved
/// session uses a per-process (and per-session) temp directory.
pub fn memory_report_dir(session_file: Option<&str>, session_id: Option<&str>) -> PathBuf {
    match session_file {
        None => {
            let suffix = session_id.map_or_else(String::new, |id| format!("-{id}"));
            std::env::temp_dir().join(format!(
                "senpi-memory-report-{}{suffix}",
                std::process::id()
            ))
        }
        Some(file) => {
            let base = file.strip_suffix(".jsonl").unwrap_or(file);
            PathBuf::from(format!("{base}-artifacts")).join("memory")
        }
    }
}

/// Writes one report per session into its artifacts directory. A failure is one stderr line and a failed outcome
/// for that session, never a panic: the report must not take the session down.
pub async fn write_memory_reports(
    sources: &[std::sync::Arc<dyn MemoryReportSessionSource>],
) -> Vec<MemoryReportOutcome> {
    let now = chrono::Utc::now();
    let taken_at = now.to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let stamp = taken_at.replace(':', "-");
    let mut outcomes = Vec::new();
    for source in sources {
        let dir = memory_report_dir(source.session_file().as_deref(), Some(&source.session_id()));
        let path = dir.join(format!("{stamp}.json"));
        let report = build_memory_report(source.as_ref(), None, &taken_at);
        let written = async {
            tokio::fs::create_dir_all(&dir).await?;
            let mut text = serde_json::to_string_pretty(&report).map_err(std::io::Error::other)?;
            text.push('\n');
            let temporary = dir.join(format!("{stamp}.json.tmp"));
            tokio::fs::write(&temporary, text).await?;
            tokio::fs::rename(&temporary, &path).await
        }
        .await;
        outcomes.push(match written {
            Ok(()) => MemoryReportOutcome::Written {
                path: path.to_string_lossy().into_owned(),
                heap_snapshot: None,
            },
            Err(error) => {
                eprintln!(
                    "senpi memory report: session {} not written: {error}",
                    source.session_id()
                );
                MemoryReportOutcome::Failed {
                    error: error.to_string(),
                }
            }
        });
    }
    outcomes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_resident_store_size::ResidentStoreSize;

    struct Saved(String);
    impl MemoryReportSessionSource for Saved {
        fn session_id(&self) -> String {
            "s".into()
        }
        fn session_file(&self) -> Option<String> {
            Some(self.0.clone())
        }
        fn resident_store(&self) -> ResidentStoreSize {
            ResidentStoreSize::default()
        }
        fn reporters(&self) -> Vec<maho_ext_api::NamedMemoryReporter> {
            Vec::new()
        }
    }

    #[test]
    fn a_saved_session_reports_beside_its_artifacts() {
        assert_eq!(
            memory_report_dir(Some("/s/a.jsonl"), Some("x")),
            PathBuf::from("/s/a-artifacts/memory")
        );
        assert!(
            memory_report_dir(None, Some("x"))
                .to_string_lossy()
                .ends_with(&format!("senpi-memory-report-{}-x", std::process::id()))
        );
    }

    #[tokio::test]
    async fn writes_one_parseable_report_per_session() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("a.jsonl").to_string_lossy().into_owned();
        let outcomes =
            write_memory_reports(&[std::sync::Arc::new(Saved(file)) as std::sync::Arc<_>]).await;
        let [
            MemoryReportOutcome::Written {
                path,
                heap_snapshot,
            },
        ] = outcomes.as_slice()
        else {
            panic!("one written report: {outcomes:?}");
        };
        assert!(heap_snapshot.is_none());
        assert!(
            path.starts_with(
                &temp
                    .path()
                    .join("a-artifacts")
                    .join("memory")
                    .to_string_lossy()
                    .into_owned()
            )
        );
        let report: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(report["sessionId"], "s");
    }
}
