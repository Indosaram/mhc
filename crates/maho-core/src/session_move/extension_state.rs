//! Per-attempt extension state capture and redeliverable restoration.
use super::bundle::parse_move_id;
use super::ownership::{SESSION_MOVED_IN, SESSION_RELEASED};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
pub const EXTENSION_STATE_PAYLOAD: &str = "extension-state";
pub const MOVED_STATE_RESTORED: &str = "session_move_state_restored";
pub const STATE_ATTEMPT_FIELD: &str = "state_attempt";
fn new_attempt() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..16].to_owned()
}
fn checked(id: &str) -> Result<String, String> {
    parse_move_id(&json!(id)).map_err(|e| e.to_string())
}
pub fn outgoing_state_dir(file: &Path, move_id: &str) -> Result<PathBuf, String> {
    Ok(file
        .parent()
        .unwrap_or(Path::new("."))
        .join(".move-state/out")
        .join(checked(move_id)?))
}
pub fn moved_state_dir(file: &Path, move_id: &str) -> Result<PathBuf, String> {
    Ok(file
        .parent()
        .unwrap_or(Path::new("."))
        .join(".move-state/in")
        .join(format!(
            "{}.{}",
            file.file_stem().unwrap_or_default().to_string_lossy(),
            checked(move_id)?
        )))
}
fn sealed_attempt(entries: &[Value], move_id: &str) -> Option<String> {
    entries
        .iter()
        .rev()
        .find(|entry| {
            entry["type"] == "custom"
                && entry["customType"] == SESSION_RELEASED
                && entry["data"]["move_id"] == move_id
        })
        .and_then(|entry| entry["data"][STATE_ATTEMPT_FIELD].as_str())
        .filter(|id| super::bundle::is_plain_id_str(id))
        .map(str::to_owned)
}
pub fn sealed_outgoing_state_dir(file: &Path, move_id: &str) -> Result<Option<PathBuf>, String> {
    let root = outgoing_state_dir(file, move_id)?;
    Ok(sealed_attempt(&super::entries(file), move_id).map(|attempt| root.join(attempt)))
}
#[derive(Debug, Clone)]
pub struct OutgoingState {
    pub attempt: String,
    pub dir: PathBuf,
}
pub fn prepare_outgoing_state(file: &Path, move_id: &str) -> Result<OutgoingState, String> {
    let root = outgoing_state_dir(file, move_id)?;
    let sealed = sealed_attempt(&super::entries(file), move_id);
    if root.exists() {
        for entry in std::fs::read_dir(&root).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            if Some(entry.file_name().to_string_lossy().as_ref()) != sealed.as_deref() {
                super::remove(&entry.path())?;
            }
        }
    }
    let attempt = new_attempt();
    let dir = root.join(&attempt);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(OutgoingState { attempt, dir })
}
#[derive(Debug, Clone)]
pub struct PendingMovedState {
    pub move_id: String,
    pub dir: PathBuf,
}
pub fn pending_moved_state(file: &Path) -> Result<Option<PendingMovedState>, String> {
    let entries = super::entries(file);
    let Some(move_id) = entries
        .iter()
        .rev()
        .find(|entry| entry["type"] == "custom" && entry["customType"] == SESSION_MOVED_IN)
        .and_then(|entry| entry["data"]["move_id"].as_str())
    else {
        return Ok(None);
    };
    if entries.iter().any(|entry| {
        entry["type"] == "custom"
            && entry["customType"] == MOVED_STATE_RESTORED
            && entry["data"]["move_id"] == move_id
    }) {
        return Ok(None);
    }
    let dir = moved_state_dir(file, move_id)?;
    Ok(dir.exists().then(|| PendingMovedState {
        move_id: move_id.into(),
        dir,
    }))
}
#[derive(Debug, Clone)]
pub struct MovedStateDelivery {
    pub move_id: String,
    pub dir: PathBuf,
    pub redelivered: bool,
}
pub async fn restore_moved_state<F, Fut>(file: &Path, deliver: F) -> Result<(), String>
where
    F: FnOnce(MovedStateDelivery) -> Fut,
    Fut: std::future::Future<Output = Result<bool, String>>,
{
    restore_moved_state_with_mark(file, deliver, |data| {
        super::append_custom(file, MOVED_STATE_RESTORED, data)
    })
    .await
}

pub async fn restore_moved_state_with_mark<F, Fut, Mark>(
    file: &Path,
    deliver: F,
    mark: Mark,
) -> Result<(), String>
where
    F: FnOnce(MovedStateDelivery) -> Fut,
    Fut: std::future::Future<Output = Result<bool, String>>,
    Mark: FnOnce(Value) -> Result<(), String>,
{
    let Some(pending) = pending_moved_state(file)? else {
        return Ok(());
    };
    let bookkeeping = pending
        .dir
        .parent()
        .and_then(Path::parent)
        .ok_or("invalid moved state path")?
        .join("delivery")
        .join(pending.dir.file_name().ok_or("invalid moved state name")?);
    let marker = bookkeeping.join("delivered");
    let redelivered = marker.exists();
    if bookkeeping.exists() {
        for entry in std::fs::read_dir(&bookkeeping).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            if entry.file_name().to_string_lossy().starts_with("attempt-") {
                super::remove(&entry.path())?;
            }
        }
    }
    std::fs::create_dir_all(&bookkeeping).map_err(|e| e.to_string())?;
    std::fs::write(marker, "").map_err(|e| e.to_string())?;
    let copy = bookkeeping.join(format!("attempt-{}", new_attempt()));
    std::fs::create_dir_all(&copy).map_err(|e| e.to_string())?;
    for from in super::files_under(&pending.dir)? {
        super::copy(
            &from,
            &copy.join(from.strip_prefix(&pending.dir).map_err(|e| e.to_string())?),
        )?;
    }
    let result = deliver(MovedStateDelivery {
        move_id: pending.move_id.clone(),
        dir: copy.clone(),
        redelivered,
    })
    .await;
    super::remove(&copy)?;
    if !result? {
        return Ok(());
    }
    mark(
        json!({"move_id":pending.move_id,"restored_at":crate::session_manager::now_iso_millis()}),
    )?;
    super::remove(&pending.dir)?;
    super::remove(&bookkeeping)
}
