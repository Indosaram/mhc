use std::path::Path;
use super::super::run_artifacts::{ArtifactError, ChildExit, RunLaunchManifest, RunOutcome, read_run_json, unlink_run_artifact, write_run_json_atomic};
pub fn run(run_dir: &Path) -> Result<i32, ArtifactError> {
    let path = run_dir.join("launch.json");
    let launch: RunLaunchManifest = read_run_json(&path)?;
    write_run_json_atomic(&run_dir.join("outcome.json"), &RunOutcome {
        version: 1, run_id: launch.run_id, attempt: Some(launch.attempt),
        finished_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        child_exit: ChildExit { code: None, signal: Some("SIGTERM".into()) }, timed_out: true,
    }, 0o600)?;
    unlink_run_artifact(&path)?;
    Ok(1)
}
