//! Resolving a session reference against a host (senpi `rpc/host-session-ref.ts`). Routing handles last one
//! host epoch; a durable id or path is kept across parking and handoff, and is resolved anew every time.
use crate::session_path_key::canonical_session_path;
use maho_core::session_manager::SessionManager;
use serde_json::Value;
use std::collections::BTreeSet;
use std::path::Path;

pub type SessionRef = String;

/// Where a reference points. `Parked` is a session the host no longer lists (idle park, handoff) whose
/// transcript still exists; `open_session { sessionPath }` resumes it.
#[derive(Clone, Debug, PartialEq)]
pub enum ResolvedSessionRef {
    /// A `list_sessions` row.
    Live(Value),
    Parked {
        session_path: String,
        cwd: String,
        durable_session_id: String,
    },
    Ambiguous(Vec<String>),
    Unknown,
}

struct Transcript {
    path: String,
    cwd: String,
    id: String,
}

fn field<'a>(row: &'a Value, key: &str) -> Option<&'a str> {
    row.get(key).and_then(Value::as_str)
}

/// `rows` is the host's `list_sessions` listing, fetched by the caller for this resolution.
pub fn resolve_session_ref(
    rows: &[Value],
    reference: &str,
    agent_dir: &Path,
) -> ResolvedSessionRef {
    if let Some(handle) = rows
        .iter()
        .find(|row| field(row, "sessionId") == Some(reference))
    {
        return ResolvedSessionRef::Live(handle.clone());
    }
    if is_session_id(reference)
        && let Some(by_id) = resolve_durable_id(reference, rows, agent_dir)
    {
        return by_id;
    }
    let named: Vec<&Value> = rows
        .iter()
        .filter(|row| field(row, "name") == Some(reference))
        .collect();
    if named.len() > 1 {
        return ResolvedSessionRef::Ambiguous(
            named
                .iter()
                .map(|row| field(row, "sessionId").unwrap_or_default().to_owned())
                .collect(),
        );
    }
    let live = named.first().copied().or_else(|| {
        rows.iter()
            .find(|row| field(row, "sessionPath").is_some_and(|path| same_path(path, reference)))
    });
    if let Some(live) = live {
        return ResolvedSessionRef::Live(live.clone());
    }
    if looks_like_path(reference) {
        parked_by_path(reference)
    } else {
        ResolvedSessionRef::Unknown
    }
}

/// A durable id names ONE session only when every live row and every transcript carrying it is the same
/// file: two transcripts with one id are refused rather than resolved to whichever is listed first.
fn resolve_durable_id(id: &str, rows: &[Value], agent_dir: &Path) -> Option<ResolvedSessionRef> {
    let live_rows: Vec<&Value> = rows
        .iter()
        .filter(|row| field(row, "durableSessionId") == Some(id))
        .collect();
    let transcripts: Vec<Transcript> = transcripts_under(agent_dir)
        .into_iter()
        .filter(|session| session.id == id)
        .collect();
    let mut files = BTreeSet::new();
    for row in &live_rows {
        files.insert(match field(row, "sessionPath") {
            Some(path) => canonical(path),
            None => field(row, "sessionId").unwrap_or_default().to_owned(),
        });
    }
    for transcript in &transcripts {
        files.insert(canonical(&transcript.path));
    }
    if files.len() > 1 {
        return Some(ResolvedSessionRef::Ambiguous(files.into_iter().collect()));
    }
    if let Some(row) = live_rows.first() {
        return Some(ResolvedSessionRef::Live((*row).clone()));
    }
    transcripts.into_iter().next().map(parked)
}

/// senpi `assertValidSessionId`: alphanumerics, `-`, `_` and `.`, starting and ending alphanumeric.
fn is_session_id(reference: &str) -> bool {
    let bytes = reference.as_bytes();
    !bytes.is_empty()
        && bytes[0].is_ascii_alphanumeric()
        && bytes[bytes.len() - 1].is_ascii_alphanumeric()
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn looks_like_path(reference: &str) -> bool {
    reference.contains('/') || reference.contains('\\') || reference.ends_with(".jsonl")
}

fn canonical(path: &str) -> String {
    canonical_session_path(Path::new(path))
        .to_string_lossy()
        .into_owned()
}

fn same_path(left: &str, right: &str) -> bool {
    canonical(left) == canonical(right)
}

fn parked_by_path(reference: &str) -> ResolvedSessionRef {
    let path = canonical_session_path(Path::new(reference));
    if !path.exists() {
        return ResolvedSessionRef::Unknown;
    }
    let Some(dir) = path.parent() else {
        return ResolvedSessionRef::Unknown;
    };
    let path = path.to_string_lossy().into_owned();
    SessionManager::list_all(Some(&dir.to_string_lossy()), None)
        .into_iter()
        .find(|session| same_path(&session.path, &path))
        .map_or(ResolvedSessionRef::Unknown, |info| {
            parked(Transcript {
                path: info.path,
                cwd: info.cwd,
                id: info.id,
            })
        })
}

/// The host keeps every transcript under `<agentDir>/sessions/<cwd-dir>/`.
fn transcripts_under(agent_dir: &Path) -> Vec<Transcript> {
    let Ok(entries) = std::fs::read_dir(agent_dir.join("sessions")) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .flat_map(|entry| SessionManager::list_all(Some(&entry.path().to_string_lossy()), None))
        .map(|info| Transcript {
            path: info.path,
            cwd: info.cwd,
            id: info.id,
        })
        .collect()
}

fn parked(info: Transcript) -> ResolvedSessionRef {
    ResolvedSessionRef::Parked {
        session_path: info.path,
        cwd: info.cwd,
        durable_session_id: info.id,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn handles_names_and_paths_resolve_to_live_rows() {
        let dir = tempfile::tempdir().expect("tempdir");
        let rows = vec![
            json!({"sessionId":"h1","name":"alpha","sessionPath":"/w/a.jsonl","durableSessionId":"d1"}),
            json!({"sessionId":"h2","name":"twin"}),
            json!({"sessionId":"h3","name":"twin"}),
        ];
        assert_eq!(
            resolve_session_ref(&rows, "h1", dir.path()),
            ResolvedSessionRef::Live(rows[0].clone())
        );
        assert_eq!(
            resolve_session_ref(&rows, "alpha", dir.path()),
            ResolvedSessionRef::Live(rows[0].clone())
        );
        assert_eq!(
            resolve_session_ref(&rows, "d1", dir.path()),
            ResolvedSessionRef::Live(rows[0].clone())
        );
        assert_eq!(
            resolve_session_ref(&rows, "twin", dir.path()),
            ResolvedSessionRef::Ambiguous(vec!["h2".into(), "h3".into()])
        );
        assert_eq!(
            resolve_session_ref(&rows, "/w/a.jsonl", dir.path()),
            ResolvedSessionRef::Live(rows[0].clone())
        );
        assert_eq!(
            resolve_session_ref(&rows, "nobody", dir.path()),
            ResolvedSessionRef::Unknown
        );
        assert_eq!(
            resolve_session_ref(&rows, "/missing/x.jsonl", dir.path()),
            ResolvedSessionRef::Unknown
        );
    }

    #[test]
    fn one_durable_id_on_two_files_is_ambiguous() {
        let dir = tempfile::tempdir().expect("tempdir");
        let rows = vec![
            json!({"sessionId":"h1","sessionPath":"/w/a.jsonl","durableSessionId":"d1"}),
            json!({"sessionId":"h2","sessionPath":"/w/b.jsonl","durableSessionId":"d1"}),
        ];
        assert!(
            matches!(resolve_session_ref(&rows, "d1", dir.path()), ResolvedSessionRef::Ambiguous(files) if files.len() == 2)
        );
        assert!(!is_session_id("-x") && is_session_id("a.b-c_d"));
    }
}
