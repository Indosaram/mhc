//! Port of senpi packages/coding-agent/src/core/foreign-session-holders.ts.

use std::path::Path;

use crate::schedule::Result;
use crate::session_holder_records::{SessionHolder, SessionHolderProbes, live_session_holders};

/// Read only the header: checking admission must not load or mutate a transcript.
fn session_id_from_file(session_file: &str) -> Result<Option<String>> {
    use std::io::BufRead;
    // Admission must not consume a pipe: its reader belongs to the session runtime.
    let metadata = match std::fs::metadata(session_file) {
        Ok(metadata) => metadata,
        // A path that does not yet exist is a new session, not a held resume.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if !metadata.is_file() {
        return Ok(None);
    }
    for line in std::io::BufReader::new(std::fs::File::open(session_file)?).lines() {
        let Some(entry) = crate::session_record::parse_entry_line(&line?) else {
            continue;
        };
        let is_header = entry.get("type").and_then(serde_json::Value::as_str) == Some("session");
        return Ok(entry
            .get("id")
            .and_then(serde_json::Value::as_str)
            .filter(|_| is_header)
            .map(str::to_owned));
    }
    Ok(None)
}

/// Fresh lease evidence for each admission; a clean exit or stale identity clears the next retry.
pub async fn foreign_session_holders(
    session_file: Option<&str>,
    session_id: Option<&str>,
    probes: &dyn SessionHolderProbes,
) -> Result<Vec<SessionHolder>> {
    let Some(session_file) = session_file else {
        return Ok(Vec::new());
    };
    let path = crate::session_manager::resolve_moved_session_path(session_file);
    let id = match session_id {
        Some(id) => id.to_owned(),
        None => match session_id_from_file(&path)? {
            Some(id) => id,
            None => return Ok(Vec::new()),
        },
    };
    let own_pid = std::process::id();
    Ok(live_session_holders(Path::new(&path), &id, probes)
        .await?
        .into_iter()
        .filter(|holder| holder.pid != own_pid)
        .collect())
}

/// One terminal-safe line; cwd can contain newlines or escape sequences.
pub async fn session_holder_warning(
    session_file: Option<&str>,
    session_id: Option<&str>,
    probes: &dyn SessionHolderProbes,
) -> Option<String> {
    let Ok(holders) = foreign_session_holders(session_file, session_id, probes).await else {
        return Some(
            "Unable to inspect session holders; continuing without holder information.".to_owned(),
        );
    };
    if holders.is_empty() {
        return None;
    }
    let who: Vec<String> = holders
        .iter()
        .map(|holder| match &holder.cwd {
            Some(cwd) => format!(
                "pid {} in {}",
                holder.pid,
                serde_json::Value::from(cwd.as_str())
            ),
            None => format!("pid {}", holder.pid),
        })
        .collect();
    Some(format!(
        "This session is also open in another process ({}). Concurrent writes may corrupt its history; quit the other process before continuing.",
        who.join("; ")
    ))
}
