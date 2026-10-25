//! Sender finalization, including receiver revocation and stable-id input recovery.
use super::move_ledger::MoveStatus;
use super::move_steps::{MoveStep, move_step};
use super::ownership::*;
use serde_json::json;
use std::path::Path;
pub struct FinishMoveOptions<'a> {
    pub session_file: &'a Path,
    pub bundle_dir: &'a Path,
    pub move_id: &'a str,
    pub answer: &'a MoveStatus,
    pub guard: Option<&'a dyn Fn() -> Result<(), String>>,
}
pub async fn finish_move(options: FinishMoveOptions<'_>) -> Result<(), String> {
    super::session_lock::with_session_move_lock(
        options.session_file,
        settle_move(&options),
        options.guard,
    )
    .await
}
async fn settle_move(options: &FinishMoveOptions<'_>) -> Result<(), String> {
    let FinishMoveOptions {
        session_file,
        bundle_dir,
        move_id,
        answer,
        ..
    } = options;
    if bundle_dir.exists() {
        let manifest = super::bundle::read_manifest(bundle_dir).map_err(|e| e.to_string())?;
        if manifest.move_id != *move_id {
            return Err(format!(
                "move_mismatch: the kept bundle belongs to {} not {move_id}",
                manifest.move_id
            ));
        }
    }
    let entries = super::entries(session_file);
    let owner = match session_ownership(&entries) {
        SessionOwnership::Writable => {
            if entries.iter().any(|e| {
                e["type"] == "custom"
                    && e["customType"] == SESSION_RECLAIMED
                    && e["data"]["move_id"] == *move_id
            }) {
                requeue_dropped_input(session_file, bundle_dir).await?;
            }
            return discard_move_files(session_file, bundle_dir, move_id);
        }
        SessionOwnership::Refused(owner) => owner,
    };
    if owner.move_id != *move_id {
        return Err(format!(
            "move_mismatch: session belongs to {} not {move_id}",
            owner.move_id
        ));
    }
    if owner.state == RefusedState::Moved {
        if matches!(answer, MoveStatus::AcceptedRevoked) {
            return Err(format!(
                "move_already_moved: receiver revoked {move_id}, but this session was already marked moved; no copy is writable"
            ));
        }
        return discard_move_files(session_file, bundle_dir, move_id);
    }
    let session_id = header_session_id(&entries).unwrap_or_default();
    match answer {
        MoveStatus::Receiving => return Ok(()),
        MoveStatus::Accepted => {
            super::append_custom(session_file, SESSION_MOVED, json!({"move_id":move_id,"session_id":session_id,"target":owner.target,"moved_at":crate::session_manager::now_iso_millis()}))?;
            move_step(MoveStep::FinishRecorded)?;
        },
        MoveStatus::AcceptedRevoked => take_back(session_file, bundle_dir, json!({"move_id":move_id,"session_id":session_id,"reason":"revoked"})).await?,
        MoveStatus::Declined { reason } => take_back(session_file, bundle_dir, json!({"move_id":move_id,"session_id":session_id,"reason":"declined","detail":reason.as_str()})).await?,
        MoveStatus::Failed { reason } => take_back(session_file, bundle_dir, json!({"move_id":move_id,"session_id":session_id,"reason":"failed","detail":reason.as_str()})).await?,
    }
    move_step(MoveStep::FinishRequeued)?;
    discard_move_files(session_file, bundle_dir, move_id)
}
async fn take_back(file: &Path, bundle: &Path, mut data: serde_json::Value) -> Result<(), String> {
    data["reclaimed_at"] = json!(crate::session_manager::now_iso_millis());
    super::append_custom(file, SESSION_RECLAIMED, data)?;
    move_step(MoveStep::FinishRecorded)?;
    requeue_dropped_input(file, bundle).await
}
fn discard_move_files(file: &Path, bundle: &Path, move_id: &str) -> Result<(), String> {
    super::remove(bundle)?;
    super::remove(&super::extension_state::outgoing_state_dir(file, move_id)?)
}
pub async fn requeue_dropped_input(file: &Path, bundle: &Path) -> Result<(), String> {
    if !bundle.exists() {
        return Ok(());
    }
    let manifest = super::bundle::read_manifest(bundle).map_err(|e| e.to_string())?;
    super::requeue::requeue_input(file, &manifest.move_id, &manifest.dropped_user_messages).await
}
