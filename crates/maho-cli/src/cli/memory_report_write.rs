use std::{
    io::Write,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

pub fn memory_report_dir(session_file: Option<&str>, session_id: &str) -> PathBuf {
    match session_file {
        Some(file) => PathBuf::from(format!(
            "{}-artifacts",
            file.strip_suffix(".jsonl").unwrap_or(file)
        ))
        .join("memory"),
        None => std::env::temp_dir().join(format!(
            "senpi-memory-report-{}-{session_id}",
            std::process::id()
        )),
    }
}
fn write_report(dir: &Path, stamp: &str, report: &serde_json::Value) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dir).map_err(|error| error.to_string())?;
    let path = dir.join(format!("{}.json", stamp.replace(':', "-")));
    let mut file = tempfile::NamedTempFile::new_in(dir).map_err(|error| error.to_string())?;
    writeln!(
        file,
        "{}",
        serde_json::to_string_pretty(report).map_err(|error| error.to_string())?
    )
    .map_err(|error| error.to_string())?;
    file.persist(&path).map_err(|error| error.to_string())?;
    Ok(path)
}
#[derive(Default)]
pub struct MemoryReportOutcome {
    #[cfg(windows)]
    pub paths: Vec<PathBuf>,
}

pub fn write_memory_reports(
    sessions: &[maho_core::agent_session::AgentSession],
) -> MemoryReportOutcome {
    #[cfg(windows)]
    let mut outcome = MemoryReportOutcome::default();
    if sessions.is_empty() {
        maho_core::output_guard::maho_write_stderr(
            "mhc memory report: no live session to report\n",
        );
    }
    for session in sessions {
        let result = (|| -> Result<PathBuf, String> {
            let elapsed = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|error| error.to_string())?;
            let stamp = maho_ext_goal::format::iso_timestamp(elapsed.as_secs())
                .ok_or("Invalid report time")?
                .replace(".000Z", &format!(".{:03}Z", elapsed.subsec_millis()));
            let report = super::memory_report_build::build_memory_report(session, &stamp)?;
            write_report(
                &memory_report_dir(session.session_file().as_deref(), &session.session_id()),
                &stamp,
                &report,
            )
        })();
        match result {
            Ok(path) => {
                #[cfg(windows)]
                outcome.paths.push(path);
                #[cfg(not(windows))]
                drop(path);
            }
            Err(error) => {
                maho_core::output_guard::maho_write_stderr(&format!(
                    "mhc memory report: session {} not written: {error}\n",
                    session.session_id()
                ));
            }
        }
    }
    #[cfg(windows)]
    {
        outcome
    }
    #[cfg(not(windows))]
    {
        MemoryReportOutcome::default()
    }
}
pub fn install_memory_report_signal() -> Result<(), String> {
    #[cfg(unix)]
    {
        let mut signal =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::user_defined2())
                .map_err(|error| error.to_string())?;
        tokio::spawn(async move {
            while signal.recv().await.is_some() {
                write_memory_reports(&super::memory_report_registry::sessions());
            }
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn saved_and_unsaved_sessions_have_distinct_artifact_directories() {
        assert_eq!(
            memory_report_dir(Some("/tmp/session.jsonl"), "id"),
            PathBuf::from("/tmp/session-artifacts/memory")
        );
        assert_ne!(
            memory_report_dir(None, "first"),
            memory_report_dir(None, "second")
        );
    }
    #[test]
    fn report_is_atomically_published_as_json() {
        let dir = tempfile::tempdir().expect("directory");
        let value =
            serde_json::json!({"sessionId":"test","residentStore":{"entries":3,"approxBytes":100}});
        let path = write_report(dir.path(), "2026-10-20T01:02:03.000Z", &value).expect("report");
        let saved: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).expect("artifact")).expect("JSON");
        assert_eq!(saved, value);
        assert_eq!(std::fs::read_dir(dir.path()).expect("entries").count(), 1);
    }
}
