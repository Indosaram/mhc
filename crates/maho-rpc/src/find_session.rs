//! Port of senpi `packages/coding-agent/src/modes/rpc/find-session.ts`.
//!
//! `find_session` (senpi#6429): locates the unique session file of this host carrying a durable
//! session id, read-only.

use crate::session_path_key::canonical_session_path;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};

pub const FIND_SESSION_FILE_LIMIT: usize = 50_000;
const HEADER_READ_BYTES: usize = 65_536;
const RPC_ERROR_FIND_SESSION_UNSUPPORTED: &str = "find_session_unsupported";
const RPC_ERROR_FIND_SESSION_TOO_MANY: &str = "find_session_too_many";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostedSession {
    pub durable_session_id: Option<String>,
    pub session_path: Option<String>,
    pub cwd: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FindSessionCandidate {
    pub session_path: String,
    pub cwd: String,
    pub open: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FindSessionAnswer {
    Unknown,
    Ambiguous,
    Found(FindSessionCandidate),
}

impl FindSessionAnswer {
    pub fn to_value(&self) -> Value {
        match self {
            Self::Unknown => json!({ "unknown": true }),
            Self::Ambiguous => json!({ "ambiguous": true }),
            Self::Found(c) => json!({ "found": c }),
        }
    }
}

pub trait FindSessionPort: Send + Sync {
    fn agent_dir(&self) -> Option<&Path>;
    fn hosted(&self) -> Vec<HostedSession>;
}

fn header_of(file: &Path) -> Option<(String, String)> {
    let mut handle = std::fs::File::open(file).ok()?;
    let mut buffer = vec![0u8; HEADER_READ_BYTES];
    let n = handle.read(&mut buffer).ok()?;
    let data = &buffer[..n];
    let newline = data.iter().position(|&b| b == b'\n');
    if newline.is_none() && n == HEADER_READ_BYTES {
        return None;
    }
    let slice = match newline {
        Some(pos) => &data[..pos],
        None => data,
    };
    let line = std::str::from_utf8(slice).ok()?;
    let header: Value = serde_json::from_str(line).ok()?;
    let obj = header.as_object()?;
    if obj.get("type").and_then(Value::as_str) != Some("session") {
        return None;
    }
    let id = obj.get("id").and_then(Value::as_str)?.to_owned();
    let cwd = obj
        .get("cwd")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    Some((id, cwd))
}

fn session_files(agent_dir: &Path) -> Result<Vec<PathBuf>, String> {
    let root = agent_dir.join("sessions");
    let folders = match std::fs::read_dir(&root) {
        Ok(d) => d,
        Err(_) => return Ok(Vec::new()),
    };
    let mut files = Vec::new();
    for folder in folders.flatten() {
        let is_dir = folder.file_type().is_ok_and(|t| t.is_dir());
        if !is_dir {
            continue;
        }
        let Ok(names) = std::fs::read_dir(folder.path()) else {
            continue;
        };
        for entry in names.flatten() {
            let name = entry.file_name();
            if name.to_string_lossy().ends_with(".jsonl") {
                if files.len() >= FIND_SESSION_FILE_LIMIT {
                    return Err(format!(
                        "find_session refused: more than {FIND_SESSION_FILE_LIMIT} session files under {}",
                        root.display()
                    ));
                }
                files.push(entry.path());
            }
        }
    }
    Ok(files)
}

pub async fn find_session_file(
    agent_dir: &Path,
    durable_session_id: &str,
    hosted: Vec<HostedSession>,
) -> Result<FindSessionAnswer, String> {
    let agent_dir = agent_dir.to_path_buf();
    let durable = durable_session_id.to_owned();
    tokio::task::spawn_blocking(move || {
        let files = session_files(&agent_dir)?;
        let mut by_path: HashMap<String, FindSessionCandidate> = HashMap::new();
        for row in hosted {
            if row.durable_session_id.as_deref() != Some(durable.as_str()) {
                continue;
            }
            let Some(session_path) = row.session_path else {
                continue;
            };
            let key = canonical_session_path(Path::new(&session_path))
                .to_string_lossy()
                .into_owned();
            by_path.insert(
                key,
                FindSessionCandidate {
                    session_path,
                    cwd: row.cwd.unwrap_or_default(),
                    open: true,
                },
            );
        }
        for file in files {
            let Some((id, cwd)) = header_of(&file) else {
                continue;
            };
            if id != durable {
                continue;
            }
            let file_str = file.to_string_lossy().into_owned();
            let key = canonical_session_path(&file).to_string_lossy().into_owned();
            by_path.entry(key).or_insert(FindSessionCandidate {
                session_path: file_str,
                cwd,
                open: false,
            });
        }
        match by_path.len() {
            0 => Ok(FindSessionAnswer::Unknown),
            1 => Ok(FindSessionAnswer::Found(
                by_path.into_values().next().expect("len == 1"),
            )),
            _ => Ok(FindSessionAnswer::Ambiguous),
        }
    })
    .await
    .map_err(|e| e.to_string())?
}

fn refusal(id: Option<&str>, code: &str, detail: &str) -> Value {
    json!({
        "id": id,
        "type": "response",
        "command": "find_session",
        "success": false,
        "error": code,
        "errorCode": code,
        "errorData": { "detail": detail },
    })
}

pub async fn find_session(
    port: &dyn FindSessionPort,
    id: Option<&str>,
    durable_session_id: Option<&str>,
) -> Value {
    let Some(durable_id) = durable_session_id.filter(|s| !s.trim().is_empty()) else {
        return refusal(
            id,
            "invalid_argument",
            "find_session needs durableSessionId, the session's own id.",
        );
    };
    let Some(agent_dir) = port.agent_dir() else {
        return refusal(
            id,
            RPC_ERROR_FIND_SESSION_UNSUPPORTED,
            "This host keeps no agent directory, so it holds no session files to look in.",
        );
    };
    match find_session_file(agent_dir, durable_id, port.hosted()).await {
        Ok(answer) => json!({
            "id": id,
            "type": "response",
            "command": "find_session",
            "success": true,
            "data": answer.to_value(),
        }),
        Err(detail) => refusal(id, RPC_ERROR_FIND_SESSION_TOO_MANY, &detail),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[tokio::test]
    async fn finds_closed_and_hosted_sessions() {
        let dir = tempfile::tempdir().expect("tempdir");
        let sdir = dir.path().join("sessions/proj");
        std::fs::create_dir_all(&sdir).expect("mkdir");
        let file = sdir.join("a.jsonl");
        let mut f = std::fs::File::create(&file).expect("create");
        writeln!(
            f,
            "{{\"type\":\"session\",\"id\":\"dur-1\",\"cwd\":\"/p\"}}"
        )
        .expect("write");

        let found = find_session_file(dir.path(), "dur-1", Vec::new())
            .await
            .expect("ok");
        assert!(matches!(
            found,
            FindSessionAnswer::Found(FindSessionCandidate { open: false, .. })
        ));

        let open = find_session_file(
            dir.path(),
            "dur-1",
            vec![HostedSession {
                durable_session_id: Some("dur-1".to_owned()),
                session_path: Some(file.to_string_lossy().into_owned()),
                cwd: Some("/p".to_owned()),
            }],
        )
        .await
        .expect("ok");
        assert!(matches!(
            open,
            FindSessionAnswer::Found(FindSessionCandidate { open: true, .. })
        ));

        let missing = find_session_file(dir.path(), "dur-missing", Vec::new())
            .await
            .expect("ok");
        assert_eq!(missing, FindSessionAnswer::Unknown);
    }
}
