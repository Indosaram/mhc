//! Session move: bundle format, ownership transitions and crash-safe operations.
//!
//! Every step reads and writes through asynchronous I/O so a move never blocks the runtime on a
//! whole-file read, a directory walk or a copy (senpi v2026.10.27-2, no-sync move path).
pub mod bundle;
pub mod export;
pub mod extension_state;
pub mod finish;
pub mod import;
pub mod move_landing;
pub mod move_ledger;
pub mod move_ledger_lock;
pub mod move_steps;
pub mod ownership;
pub mod requeue;
pub mod revoke;
pub mod session_lock;
pub mod write_guard;

use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use tokio::io::AsyncWriteExt;

pub(crate) async fn entries(file: &Path) -> Vec<Value> {
    crate::session_entry_file::load_entries_from_file_async(file, None).await
}
/// Move metadata bypasses ordinary write admission: the caller holds the move lock.
/// Unlike SessionManager's best-effort append, failure must abort the protocol.
pub(crate) async fn append_custom(file: &Path, kind: &str, data: Value) -> Result<(), String> {
    let parent = entries(file)
        .await
        .last()
        .and_then(|e| e.get("id"))
        .cloned();
    let entry = json!({"type":"custom", "customType":kind, "data":data, "id":uuid::Uuid::new_v4().to_string(), "parentId":parent, "timestamp":crate::session_manager::now_iso_millis()});
    append_json(file, &entry).await
}
pub(crate) async fn append_json(file: &Path, entry: &Value) -> Result<(), String> {
    let mut output = tokio::fs::OpenOptions::new()
        .append(true)
        .open(file)
        .await
        .map_err(|e| e.to_string())?;
    output
        .write_all(format!("{entry}\n").as_bytes())
        .await
        .map_err(|e| e.to_string())?;
    output.sync_all().await.map_err(|e| e.to_string())
}
/// senpi `pathExists`.
pub(crate) async fn path_exists(path: &Path) -> bool {
    tokio::fs::try_exists(path).await.unwrap_or(false)
}
pub(crate) async fn remove(path: &Path) -> Result<(), String> {
    let result = match tokio::fs::symlink_metadata(path).await {
        Ok(metadata) if metadata.is_dir() => tokio::fs::remove_dir_all(path).await,
        Ok(_) => tokio::fs::remove_file(path).await,
        Err(error) => Err(error),
    };
    match result {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}
pub(crate) async fn copy(from: &Path, to: &Path) -> Result<(), String> {
    if let Some(parent) = to.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e| e.to_string())?;
    }
    tokio::fs::copy(from, to)
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())
}
/// Every file under `dir`, in directory order with each subdirectory's files in place (senpi `filesUnder`).
pub(crate) fn files_under(
    dir: &Path,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Vec<PathBuf>, String>> + Send + '_>>
{
    Box::pin(async move {
        if !path_exists(dir).await {
            return Ok(Vec::new());
        }
        let mut files = Vec::new();
        let mut entries = tokio::fs::read_dir(dir).await.map_err(|e| e.to_string())?;
        while let Some(entry) = entries.next_entry().await.map_err(|e| e.to_string())? {
            let path = entry.path();
            if entry.file_type().await.map_err(|e| e.to_string())?.is_dir() {
                files.extend(files_under(&path).await?);
            } else {
                files.push(path);
            }
        }
        Ok(files)
    })
}
#[cfg(test)]
mod tests;
