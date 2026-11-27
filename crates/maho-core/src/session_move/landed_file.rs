//! Port of senpi packages/coding-agent/src/core/session-move/landed-file.ts.

use std::path::Path;

use serde_json::Value;

use crate::session_entry_file::load_entries_from_file_async;

use super::move_landing::LandingOutcome;
use super::ownership::{SESSION_MOVED, SESSION_MOVED_IN, SessionOwnership, session_ownership};
use super::revoke::REVOKED;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LandedFile {
    At {
        session_path: String,
        session_id: String,
        cwd: String,
        ownership: SessionOwnership,
    },
    Gone,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LandingRead {
    pub outcome: LandingOutcome,
    pub file: LandedFile,
}

/// What a session file's entries record for the move: nothing, its import, or its import and revoke.
pub fn outcome_in(entries: &[Value], move_id: &str) -> LandingOutcome {
    let mut outcome = LandingOutcome::Absent;
    for entry in entries {
        if entry.get("type").and_then(Value::as_str) != Some("custom") {
            continue;
        }
        let Some(data) = entry.get("data").and_then(Value::as_object) else {
            continue;
        };
        if data.get("move_id").and_then(Value::as_str) != Some(move_id) {
            continue;
        }
        let ct = entry.get("customType").and_then(Value::as_str);
        if ct == Some(SESSION_MOVED_IN) {
            outcome = LandingOutcome::MovedIn;
        } else if outcome == LandingOutcome::MovedIn
            && ct == Some(SESSION_MOVED)
            && data.get("reason").and_then(Value::as_str) == Some(REVOKED)
        {
            outcome = LandingOutcome::Revoked;
        }
    }
    outcome
}

/// One read of the file at a move's landing target.
pub async fn read_landing(target: &Path, move_id: &str) -> LandingRead {
    if !super::path_exists(target).await {
        return LandingRead {
            outcome: LandingOutcome::Absent,
            file: LandedFile::Gone,
        };
    }
    let entries = load_entries_from_file_async(target, None).await;
    let outcome = outcome_in(&entries, move_id);
    let header = entries
        .iter()
        .find(|e| e.get("type").and_then(Value::as_str) == Some("session"));
    let session_id = header
        .and_then(|h| h.get("id"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    let cwd = header
        .and_then(|h| h.get("cwd"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    match (outcome, session_id, cwd) {
        (LandingOutcome::Absent, _, _) | (_, None, _) | (_, _, None) => LandingRead {
            outcome,
            file: LandedFile::Gone,
        },
        (_, Some(session_id), Some(cwd)) => LandingRead {
            outcome,
            file: LandedFile::At {
                session_path: target.to_string_lossy().into_owned(),
                session_id: session_id.to_owned(),
                cwd: cwd.to_owned(),
                ownership: session_ownership(&entries),
            },
        },
    }
}
