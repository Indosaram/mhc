//! Port of senpi packages/coding-agent/src/core/session-move/copy.ts.

use std::path::Path;

use serde_json::Value;

use crate::session_entry_file::load_entries_from_file_async;
use crate::session_manager::{NewSessionOptions, SessionManager, session_file_name_id};
use crate::session_move::ownership::header_session_id;
use crate::session_sidecar_store::serialize_by_key;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionCopyRefusalCode {
    MoveTargetOccupied,
    InvalidMoveRequest,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{detail}")]
pub struct SessionCopyRefusal {
    pub code: SessionCopyRefusalCode,
    pub detail: String,
}

impl SessionCopyRefusal {
    pub fn code_str(&self) -> &'static str {
        match self.code {
            SessionCopyRefusalCode::MoveTargetOccupied => "move_target_occupied",
            SessionCopyRefusalCode::InvalidMoveRequest => "invalid_move_request",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CopySessionResult {
    pub session_id: String,
    pub session_path: String,
}

async fn id_taken(dir: &Path, session_id: &str) -> std::io::Result<bool> {
    let mut rd = match tokio::fs::read_dir(dir).await {
        Ok(rd) => rd,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(err) => return Err(err),
    };
    while let Some(entry) = rd.next_entry().await? {
        let name = entry.file_name();
        if let Some(name_str) = name.to_str()
            && session_file_name_id(name_str).as_deref() == Some(session_id)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Copies a session file into a new session under a caller-chosen id (`senpi#5889`).
pub async fn copy_session_as(
    source_file: &Path,
    session_id: &str,
    session_dir_for_cwd: impl FnOnce(&str) -> String,
) -> Result<CopySessionResult, String> {
    let entries = load_entries_from_file_async(source_file, None).await;
    let cwd = entries
        .iter()
        .find(|e| e.get("type").and_then(Value::as_str) == Some("session"))
        .and_then(|h| h.get("cwd"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| {
            format!(
                "The session at {} has no working directory in its header.",
                source_file.display()
            )
        })?;
    if header_session_id(&entries) == Some(session_id) {
        return Err(SessionCopyRefusal {
            code: SessionCopyRefusalCode::InvalidMoveRequest,
            detail: "The copy needs a new session id: under the source's own id it would stay read-only.".to_owned(),
        }.to_string());
    }
    let dir = session_dir_for_cwd(&cwd);
    let key = format!("session-copy:{dir}:{session_id}");
    let source = source_file.to_string_lossy().into_owned();
    let target_id = session_id.to_owned();
    serialize_by_key(&key, async move {
        if id_taken(Path::new(&dir), &target_id)
            .await
            .map_err(|e| e.to_string())?
        {
            return Err(SessionCopyRefusal {
                code: SessionCopyRefusalCode::MoveTargetOccupied,
                detail: format!(
                    "A session with id {target_id} already exists in {dir}. Copy under another id."
                ),
            }
            .to_string());
        }
        let copy = SessionManager::fork_from(
            &source,
            &cwd,
            Some(&dir),
            Some(NewSessionOptions {
                id: Some(target_id.clone()),
                ..Default::default()
            }),
        )?;
        let session_path = copy
            .session_file()
            .map(str::to_owned)
            .ok_or_else(|| "The copy was created without a session file.".to_owned())?;
        Ok(CopySessionResult {
            session_id: target_id,
            session_path,
        })
    })
    .await
}
