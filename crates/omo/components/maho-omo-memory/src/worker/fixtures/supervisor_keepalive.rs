use std::path::Path;
pub async fn run(
    run_dir: &Path, executable: &Path, prefix: &[String],
    gate: impl FnOnce(&mut dyn FnMut() -> Result<(), String>) -> Result<(), String>,
) -> Result<(), String> {
    let _unrelated_handle = tokio::spawn(std::future::pending::<()>());
    super::super::memory_run_supervisor::run_supervisor(run_dir, executable, prefix, gate).await
}
