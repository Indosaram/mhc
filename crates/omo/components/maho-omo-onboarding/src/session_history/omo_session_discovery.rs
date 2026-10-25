//! Projects the omo CLI has run sessions in (upstream `omo-session-discovery.ts`).

use std::{collections::HashSet, path::Path};

use serde_json::{Map, Value};

use super::constants::{
    MAX_OMO_DISCOVERY_OPERATIONS, MAX_OMO_SESSION_HEADERS, OMO_HEADER_READ_BYTES,
    OMO_STARTUP_ONLY_MAX_BYTES,
};
use super::project_paths::resolve_path;
use super::session_history_fs::{
    FileKind, SessionHistoryFs, read_file_string_or_empty, real_path_or, stat_or_null,
};
use super::transcript_discovery::join;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OmoProjectCandidate {
    pub cwd: String,
    pub last_active_at_ms: Option<i64>,
}

#[derive(Clone, Debug, Default)]
pub struct OmoDiscovery {
    pub projects: Vec<OmoProjectCandidate>,
    pub found: usize,
    pub read: usize,
    pub truncated: bool,
}

/// Entry kinds a session records on its own while it starts; anything else happened in the session.
const STARTUP_ENTRY_TYPES: [&str; 4] = [
    "model_change",
    "thinking_level_change",
    "custom",
    "custom_message",
];

fn parse_object(line: &str) -> Option<Map<String, Value>> {
    match serde_json::from_str::<Value>(line) {
        Ok(Value::Object(map)) => Some(map),
        _ => None,
    }
}

struct Header {
    id: Option<String>,
    cwd: Option<String>,
}

fn peek_session_header(fs: &dyn SessionHistoryFs, file_path: &str) -> Header {
    let none = Header {
        id: None,
        cwd: None,
    };
    let Ok(mut file) = fs.open(file_path) else {
        return none;
    };
    let raw = file.read(OMO_HEADER_READ_BYTES);
    let _ = file.close();
    let Ok(Some(raw)) = raw else { return none };
    let end = raw.iter().position(|b| *b == b'\n').unwrap_or(raw.len());
    let Some(header) = parse_object(&String::from_utf8_lossy(&raw[..end])) else {
        return none;
    };
    let field = |key: &str| match header.get(key) {
        None => Ok(None),
        Some(Value::String(text)) => Ok(Some(text.clone())),
        Some(_) => Err(()),
    };
    match (field("id"), field("cwd")) {
        (Ok(id), Ok(cwd)) => Header { id, cwd },
        _ => none,
    }
}

/// True for a readable session holding a header and nothing but startup records (an app's own probe session).
fn holds_only_startup_records(fs: &dyn SessionHistoryFs, file_path: &str) -> bool {
    let contents = read_file_string_or_empty(fs, file_path);
    let mut lines = contents.split('\n').filter(|line| !line.trim().is_empty());
    let Some(header_line) = lines.next() else {
        return false;
    };
    if parse_object(header_line)
        .and_then(|h| h.get("type").and_then(Value::as_str).map(str::to_owned))
        .as_deref()
        != Some("session")
    {
        return false;
    }
    lines.all(|line| {
        parse_object(line)
            .and_then(|entry| entry.get("type").and_then(Value::as_str).map(str::to_owned))
            .is_some_and(|kind| STARTUP_ENTRY_TYPES.contains(&kind.as_str()))
    })
}

struct SessionFile {
    file_path: String,
    mtime_ms: Option<i64>,
    size: u64,
}

fn list_session_files(fs: &dyn SessionHistoryFs, roots: &[String]) -> (Vec<SessionFile>, bool) {
    let mut operations = MAX_OMO_DISCOVERY_OPERATIONS;
    let mut seen_roots = HashSet::new();
    let mut files = Vec::new();
    let mut spend = || {
        let ok = operations > 0;
        operations -= 1;
        ok
    };
    for root in roots {
        if !spend() {
            return (files, true);
        }
        let identity = real_path_or(fs, &resolve_path(root));
        if !seen_roots.insert(identity) {
            continue;
        }
        if !spend() {
            return (files, true);
        }
        let Ok(names) = fs.read_directory(root) else {
            continue;
        };
        let mut directories = vec![root.clone()];
        directories.extend(
            names
                .iter()
                .filter(|n| n.starts_with("--") && n.ends_with("--"))
                .map(|n| join(root, n)),
        );
        for directory in directories {
            if !spend() {
                return (files, true);
            }
            let entries = if directory == *root {
                names.clone()
            } else {
                match fs.read_directory(&directory) {
                    Ok(entries) => entries,
                    Err(_) => continue,
                }
            };
            for entry in entries {
                if !entry.ends_with(".jsonl") {
                    continue;
                }
                if !spend() {
                    return (files, true);
                }
                let file_path = join(&directory, &entry);
                if let Some(stats) = stat_or_null(fs, &file_path)
                    && stats.kind == FileKind::File
                {
                    files.push(SessionFile {
                        file_path,
                        mtime_ms: stats.mtime_ms,
                        size: stats.size,
                    });
                }
            }
        }
    }
    (files, false)
}

/// Projects the omo CLI has run sessions in. A folder is offered only for a session in which something
/// happened; one confirmed session per folder is enough, so a confirmed folder is never read again.
pub fn discover_omo_cli_projects(fs: &dyn SessionHistoryFs, roots: &[String]) -> OmoDiscovery {
    let (mut files, listed_truncated) = list_session_files(fs, roots);
    files.sort_by(|left, right| {
        right
            .mtime_ms
            .unwrap_or(0)
            .cmp(&left.mtime_ms.unwrap_or(0))
            .then_with(|| left.file_path.cmp(&right.file_path))
    });
    let found = files.len();
    let selected = &files[..found.min(MAX_OMO_SESSION_HEADERS)];
    let mut projects = Vec::new();
    let mut confirmed_cwds = HashSet::new();
    for file in selected {
        let header = peek_session_header(fs, &file.file_path);
        let (Some(id), Some(cwd)) = (header.id, header.cwd) else {
            continue;
        };
        if id.trim().is_empty() || !Path::new(&cwd).is_absolute() {
            continue;
        }
        if !confirmed_cwds.contains(&cwd)
            && file.size <= OMO_STARTUP_ONLY_MAX_BYTES
            && holds_only_startup_records(fs, &file.file_path)
        {
            continue;
        }
        confirmed_cwds.insert(cwd.clone());
        projects.push(OmoProjectCandidate {
            cwd,
            last_active_at_ms: file.mtime_ms,
        });
    }
    OmoDiscovery {
        projects,
        found,
        read: selected.len(),
        truncated: listed_truncated || found > MAX_OMO_SESSION_HEADERS,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_history::session_history_fs::NodeSessionHistoryFs;

    fn write(dir: &std::path::Path, name: &str, lines: &[&str]) -> Result<(), std::io::Error> {
        std::fs::write(dir.join(name), format!("{}\n", lines.join("\n")))
    }

    /// A session header whose cwd is absolute on this platform (`/real` is relative on Windows).
    fn header(id: &str, cwd: &str) -> String {
        serde_json::json!({ "type": "session", "id": id, "cwd": cwd }).to_string()
    }

    const PROBE: &str = if cfg!(windows) { "C:\\probe" } else { "/probe" };
    const REAL: &str = if cfg!(windows) { "C:\\real" } else { "/real" };

    #[test]
    fn startup_only_sessions_are_skipped_and_real_ones_confirm_the_folder()
    -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let root = dir.path();
        write(
            root,
            "probe.jsonl",
            &[
                header("a", PROBE).as_str(),
                r#"{"type":"model_change"}"#,
            ],
        )?;
        write(
            root,
            "real.jsonl",
            &[
                header("b", REAL).as_str(),
                r#"{"type":"message"}"#,
            ],
        )?;
        write(
            root,
            "rel.jsonl",
            &[
                r#"{"type":"session","id":"c","cwd":"relative"}"#,
                r#"{"type":"message"}"#,
            ],
        )?;
        write(
            root,
            "bad.jsonl",
            &[r#"{"type":"session","id":5,"cwd":"/x"}"#],
        )?;
        let found = discover_omo_cli_projects(
            &NodeSessionHistoryFs,
            &[root.to_string_lossy().into_owned()],
        );
        let cwds: Vec<&str> = found.projects.iter().map(|p| p.cwd.as_str()).collect();
        assert_eq!(cwds, vec![REAL]);
        assert_eq!(found.found, 4);
        assert!(!found.truncated);
        Ok(())
    }
}
