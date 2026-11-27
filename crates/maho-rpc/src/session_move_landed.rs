//! Port of senpi `packages/coding-agent/src/modes/rpc/session-move-landed.ts`.
//!
//! The `move_session` answers that say where an accepted move landed (`status`, and a
//! `move_fenced` refusal of a move already accepted here).

use crate::session_move_request::StatusNamedFile;
use maho_core::session_manager::get_default_session_dir_path;
use maho_core::session_move::import::{StatusFile, StatusFileKind, answer_move_status_and_landing};
use maho_core::session_move::landed_file::LandedFile;
use maho_core::session_move::move_ledger::{MoveFencedError, MoveLedger, MoveStatus};
use maho_core::session_move::ownership::SessionOwnership;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

pub fn wire_landed(landed: Option<&LandedFile>) -> Option<Value> {
    let landed = landed?;
    match landed {
        LandedFile::Gone => Some(json!({ "landed_file": "gone" })),
        LandedFile::At {
            session_path,
            session_id,
            cwd,
            ownership,
        } => match ownership {
            SessionOwnership::Writable => Some(json!({
                "session_path": session_path,
                "session_id": session_id,
                "cwd": cwd,
                "writable": true,
            })),
            SessionOwnership::Refused(owner) => {
                let state = match owner.state {
                    maho_core::session_move::ownership::RefusedState::Moving => "moving",
                    maho_core::session_move::ownership::RefusedState::Moved => "moved",
                };
                let mut sealed_by = json!({
                    "move_id": owner.move_id,
                    "state": state,
                });
                if !owner.target.is_empty() {
                    sealed_by["target"] = json!(owner.target);
                }
                Some(json!({
                    "session_path": session_path,
                    "session_id": session_id,
                    "cwd": cwd,
                    "writable": false,
                    "sealed_by": sealed_by,
                }))
            }
        },
    }
}

pub fn status_file_path(named: &StatusNamedFile, agent_dir: &Path) -> (PathBuf, StatusFileKind) {
    match named {
        StatusNamedFile::Path { session_path } => {
            (PathBuf::from(session_path), StatusFileKind::Copy)
        }
        StatusNamedFile::Landing {
            cwd,
            session_file_name,
            session_dir,
        } => {
            let dir = match session_dir {
                Some(d) => PathBuf::from(d),
                None => get_default_session_dir_path(cwd, &agent_dir.to_string_lossy()).into(),
            };
            (dir.join(session_file_name), StatusFileKind::Landing)
        }
    }
}

pub fn move_status_to_value(status: &MoveStatus) -> Value {
    match status {
        MoveStatus::Receiving => json!({ "state": "receiving" }),
        MoveStatus::AcceptedRevoked => json!({ "state": "accepted", "revoked": true }),
        MoveStatus::Accepted => json!({ "state": "accepted" }),
        MoveStatus::Declined { reason } => {
            json!({ "state": "declined", "reason": reason.as_str() })
        }
        MoveStatus::Failed { reason } => json!({ "state": "failed", "reason": reason.as_str() }),
    }
}

pub async fn status_answer(
    ledger: &MoveLedger,
    move_id: &str,
    named: Option<&StatusNamedFile>,
    agent_dir: &Path,
) -> Result<Value, String> {
    let resolved = named.map(|n| status_file_path(n, agent_dir));
    let asked = resolved.as_ref().map(|(file, kind)| StatusFile {
        file: file.as_path(),
        kind: *kind,
    });
    let res = answer_move_status_and_landing(ledger, move_id, asked).await?;
    let mut base = move_status_to_value(&res.answer);
    if matches!(
        res.answer,
        MoveStatus::Accepted | MoveStatus::AcceptedRevoked
    ) && let Some(Value::Object(landed_map)) = wire_landed(res.landed.as_ref())
        && let Some(base_map) = base.as_object_mut()
    {
        for (k, v) in landed_map {
            base_map.insert(k, v);
        }
    }
    Ok(base)
}

pub async fn landed_when_fenced(ledger: &MoveLedger, fenced: &MoveFencedError) -> Option<Value> {
    let res = answer_move_status_and_landing(ledger, &fenced.move_id, None)
        .await
        .ok()?;
    let mut landed = wire_landed(res.landed.as_ref())?;
    if matches!(res.answer, MoveStatus::AcceptedRevoked)
        && let Some(obj) = landed.as_object_mut()
    {
        obj.insert("revoked".to_owned(), Value::Bool(true));
    }
    Some(landed)
}
