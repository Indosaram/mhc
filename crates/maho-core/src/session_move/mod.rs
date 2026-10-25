//! Session move: bundle format, ownership transitions and crash-safe operations.
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

use serde_json::{Value, json};
use std::io::Write;
use std::path::Path;
pub(crate) fn entries(file: &Path) -> Vec<Value> {
    crate::session_manager::load_entries_from_file(&file.to_string_lossy())
}
/// Move metadata bypasses ordinary write admission: the caller holds the move lock.
/// Unlike SessionManager's best-effort append, failure must abort the protocol.
pub(crate) fn append_custom(file: &Path, kind: &str, data: Value) -> Result<(), String> {
    let parent = entries(file).last().and_then(|e| e.get("id")).cloned();
    let entry = json!({"type":"custom", "customType":kind, "data":data, "id":uuid::Uuid::new_v4().to_string(), "parentId":parent, "timestamp":crate::session_manager::now_iso_millis()});
    append_json(file, &entry)
}
pub(crate) fn append_json(file: &Path, entry: &Value) -> Result<(), String> {
    let mut output = std::fs::OpenOptions::new()
        .append(true)
        .open(file)
        .map_err(|e| e.to_string())?;
    writeln!(output, "{entry}").map_err(|e| e.to_string())?;
    output.sync_all().map_err(|e| e.to_string())
}
pub(crate) fn remove(path: &Path) -> Result<(), String> {
    let result = if path.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    };
    match result {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}
pub(crate) fn copy(from: &Path, to: &Path) -> Result<(), String> {
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    std::fs::copy(from, to)
        .map(|_| ())
        .map_err(|e| e.to_string())
}
pub(crate) fn files_under(dir: &Path) -> Result<Vec<std::path::PathBuf>, String> {
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut files = Vec::new();
    for entry in std::fs::read_dir(dir).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        if entry.file_type().map_err(|e| e.to_string())?.is_dir() {
            files.extend(files_under(&entry.path())?);
        } else {
            files.push(entry.path());
        }
    }
    Ok(files)
}
#[cfg(test)]
mod tests;
