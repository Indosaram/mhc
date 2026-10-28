//! Ownership acquisition for senpi `experimental/session-worker.ts::run`.
use std::{path::Path, sync::Arc, time::Duration};
use super::{server::{HeldFileLock, ServerStartupError}, session_worker_ownership::{OwnershipLossStop, ReportFuture, stop_worker_on_lost_ownership}};

/// The control transport sends this envelope before the worker exits. Keeping the token and key in
/// the captured closure makes lock loss during harness construction reportable as well.
pub async fn run<T, F, Fut>(
    session_path: &Path,
    token: String,
    session_key: String,
    send: Arc<dyn Fn(serde_json::Value) -> ReportFuture + Send + Sync>,
    run_owned_session: F,
) -> Result<T, ServerStartupError>
where
    F: FnOnce(HeldFileLock) -> Fut,
    Fut: std::future::Future<Output = Result<T, ServerStartupError>>,
{
    let canonical_path = std::fs::canonicalize(session_path)?;
    let stop = stop_worker_on_lost_ownership(OwnershipLossStop::new(
        move |message| send(serde_json::json!({ "type": "worker_failed", "token": token, "sessionKey": session_key, "message": message })),
        |code| std::process::exit(code),
    ));
    let ownership = HeldFileLock::acquire(
        &canonical_path,
        Duration::from_millis(2_000),
        Duration::from_millis(1_000),
        Duration::from_millis(8_000),
        Some(stop),
    ).await?;
    run_owned_session(ownership).await
}
