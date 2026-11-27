//! Port of senpi packages/coding-agent/src/core/foreign-session-holders.ts.

use std::collections::HashSet;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::schedule::Result;
pub use crate::session_holder_records::CheckedSessionHolder;
use crate::session_holder_records::{
    SessionHolder, SessionHolderProbes, live_session_holders_checked,
};

pub fn session_holders_from_holders(holders: &[CheckedSessionHolder]) -> Vec<SessionHolder> {
    holders
        .iter()
        .map(|checked| checked.holder.clone())
        .collect()
}

pub const SESSION_HOLDER_LOOKUP_FAILED: &str =
    "Unable to inspect session holders; continuing without holder information.";

/// Read only the header: checking admission must not load or mutate a transcript.
pub fn session_id_from_file(session_file: &str) -> Result<Option<String>> {
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
    Ok(
        foreign_session_holders_checked(session_file, session_id, probes)
            .await?
            .into_iter()
            .map(|checked| checked.holder)
            .collect(),
    )
}

pub async fn foreign_session_holders_checked(
    session_file: Option<&str>,
    session_id: Option<&str>,
    probes: &dyn SessionHolderProbes,
) -> Result<Vec<CheckedSessionHolder>> {
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
    Ok(live_session_holders_checked(Path::new(&path), &id, probes)
        .await?
        .into_iter()
        .filter(|checked| checked.holder.pid != own_pid)
        .collect())
}

/// One terminal-safe line; cwd can contain newlines or escape sequences.
pub fn describe_session_holders(holders: &[SessionHolder]) -> String {
    holders
        .iter()
        .map(|holder| match &holder.cwd {
            Some(cwd) => format!(
                "pid {} in {}",
                holder.pid,
                serde_json::Value::from(cwd.as_str())
            ),
            None => format!("pid {}", holder.pid),
        })
        .collect::<Vec<_>>()
        .join("; ")
}

pub enum CheckSessionHoldersOutcome {
    Holders(Vec<CheckedSessionHolder>),
    LookupFailed,
}

/// Live foreign holders of `session_file` before an interactive resume switches onto it; a failed
/// lookup stays advisory.
pub async fn check_session_holders(
    session_file: &str,
    probes: &dyn SessionHolderProbes,
) -> CheckSessionHoldersOutcome {
    match foreign_session_holders_checked(Some(session_file), None, probes).await {
        Ok(holders) => CheckSessionHoldersOutcome::Holders(holders),
        Err(_) => CheckSessionHoldersOutcome::LookupFailed,
    }
}

/// Why taking over is not offered, or `None` when it is: a holder whose identity is unverified
/// could be a reused pid, and a holder inside a multi-session host would take every other session
/// of that host down with it.
pub fn take_over_refusal(holders: &[CheckedSessionHolder], retry: Option<&str>) -> Option<String> {
    let retry = retry.unwrap_or("then /resume again");
    if holders
        .iter()
        .any(|checked| checked.holder.host_instance.is_some())
    {
        return Some(format!(
            "It is open in a multi-session host; close it there, {retry}."
        ));
    }
    if holders.iter().any(|checked| checked.unproven) {
        return Some(format!(
            "The other process could not be verified; quit it there, {retry}."
        ));
    }
    None
}

/// The socket of the one multi-session host that holds the session, when every holder is a session
/// of that host and the host published its public socket (#6059).
pub fn host_attach_socket(holders: &[CheckedSessionHolder]) -> Option<String> {
    if holders.iter().any(|checked| checked.unproven) {
        return None;
    }
    let sockets: HashSet<Option<&str>> = holders
        .iter()
        .map(|checked| {
            checked
                .holder
                .host_instance
                .as_ref()
                .and(checked.holder.host_socket.as_deref())
        })
        .collect();
    if sockets.len() != 1 {
        return None;
    }
    sockets
        .into_iter()
        .next()
        .flatten()
        .filter(|socket| !socket.is_empty())
        .map(str::to_owned)
}

/// Explicit take-over of a session another live process holds (senpi#5744).
pub async fn stop_session_holders(
    session_file: &str,
    confirmed: &[CheckedSessionHolder],
    probes: &dyn SessionHolderProbes,
    timeout: Option<Duration>,
) -> bool {
    let confirmed_pids: HashSet<u32> = confirmed.iter().map(|c| c.holder.pid).collect();
    let Ok(current) = foreign_session_holders_checked(Some(session_file), None, probes).await
    else {
        return false;
    };
    let current: Vec<CheckedSessionHolder> = current
        .into_iter()
        .filter(|c| confirmed_pids.contains(&c.holder.pid))
        .collect();
    if take_over_refusal(&current, None).is_some() {
        return false;
    }
    for checked in &current {
        send_sigterm(checked.holder.pid);
    }
    let stopping: HashSet<u32> = current.iter().map(|c| c.holder.pid).collect();
    let deadline = Instant::now() + timeout.unwrap_or(Duration::from_secs(10));
    loop {
        let Ok(holders) = foreign_session_holders(Some(session_file), None, probes).await else {
            return false;
        };
        if !holders.iter().any(|h| stopping.contains(&h.pid)) {
            return holders.is_empty();
        }
        if Instant::now() >= deadline {
            return false;
        }
        // A foreign process offers no exit event to await; its holder record is the only signal.
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[cfg(unix)]
fn send_sigterm(pid: u32) {
    if let Ok(pid) = i32::try_from(pid)
        && pid > 0
    {
        let _ = std::process::Command::new("kill")
            .args(["-TERM", &pid.to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
}

#[cfg(not(unix))]
fn send_sigterm(_pid: u32) {}

/// The post-switch advisory line, or `None` when no other process holds the session.
pub async fn session_holder_warning(
    session_file: Option<&str>,
    session_id: Option<&str>,
    probes: &dyn SessionHolderProbes,
) -> Option<String> {
    let Ok(holders) = foreign_session_holders(session_file, session_id, probes).await else {
        return Some(SESSION_HOLDER_LOOKUP_FAILED.to_owned());
    };
    if holders.is_empty() {
        return None;
    }
    Some(format!(
        "This session is also open in another process ({}). Concurrent writes may corrupt its history; quit the other process before continuing.",
        describe_session_holders(&holders)
    ))
}
