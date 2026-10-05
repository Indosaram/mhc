use std::path::Path;
pub async fn run(run_dir: &Path) -> Result<(), super::super::run_artifacts::ArtifactError> {
    super::super::run_sentinel::wait_for_run_sentinel(
        &run_dir.join("release"), chrono::Utc::now().timestamp_millis() as f64 + 30_000.0,
        || chrono::Utc::now().timestamp_millis() as f64, None,
    ).await;
    super::super::run_artifacts::write_run_json_atomic(&run_dir.join("released.json"), &serde_json::json!({"released":true}), 0o600)
}
