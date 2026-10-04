use std::path::Path;
use super::super::run_artifacts::{ArtifactError, ChildExit, RunLaunchManifest, RunOutcome, read_run_json, unlink_run_artifact, write_run_json_atomic};
pub async fn run(run_dir: &Path) -> Result<(), ArtifactError> {
    let path = run_dir.join("launch.json");
    let launch: RunLaunchManifest = read_run_json(&path)?;
    write_run_json_atomic(&run_dir.join("outcome.json"), &RunOutcome {
        version: 1, run_id: launch.run_id, attempt: Some(launch.attempt),
        finished_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        child_exit: ChildExit { code: Some(0), signal: None }, timed_out: false,
    }, 0o600)?;
    unlink_run_artifact(&path)?;
    super::supervisor_never_publishes::run(run_dir).await
}
