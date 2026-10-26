//! Receiver revocation seals the file before marking the accepted ledger answer.
use super::move_ledger::MoveLedger;
use super::move_steps::{MoveStep, move_step};
use super::ownership::*;
use serde_json::json;
use std::path::Path;
pub const REVOKED: &str = "revoked";
pub struct RevokeMoveOptions<'a> {
    pub session_file: &'a Path,
    pub move_id: &'a str,
    pub ledger: &'a MoveLedger,
    pub guard: Option<&'a dyn Fn() -> Result<(), String>>,
}
pub async fn revoke_imported_move(options: RevokeMoveOptions<'_>) -> Result<(), String> {
    super::session_lock::with_session_move_lock(
        options.session_file,
        async {
            seal(options.session_file, options.move_id)?;
            move_step(MoveStep::RevokeSealed)?;
            options
                .ledger
                .mark_revoked(options.move_id)
                .await
                .map_err(|e| e.to_string())?;
            move_step(MoveStep::RevokeMarked)
        },
        options.guard,
    )
    .await
}
fn seal(file: &Path, move_id: &str) -> Result<(), String> {
    let entries = super::entries(file);
    let session_id = header_session_id(&entries);
    let mut ownership = SessionOwnership::Writable;
    let mut latest = None;
    for entry in &entries {
        if let OwnershipStep::Apply(next) = ownership_step(&ownership, entry, session_id) {
            ownership = next;
            latest = Some(entry);
        }
    }
    let latest = latest
        .filter(|e| e["data"]["move_id"] == move_id)
        .ok_or_else(|| {
            format!("move_mismatch: this file is not the imported copy for {move_id}")
        })?;
    if latest["customType"] == SESSION_MOVED && latest["data"]["reason"] == REVOKED {
        return Ok(());
    }
    if ownership != SessionOwnership::Writable || latest["customType"] != SESSION_MOVED_IN {
        return Err(format!(
            "move_mismatch: this file is not the latest import for {move_id}"
        ));
    }
    super::append_custom(
        file,
        SESSION_MOVED,
        json!({"move_id":move_id,"session_id":session_id.unwrap_or_default(),"target":latest["data"]["from"].as_str().unwrap_or_default(),"reason":REVOKED,"moved_at":crate::session_manager::now_iso_millis()}),
    )
}
