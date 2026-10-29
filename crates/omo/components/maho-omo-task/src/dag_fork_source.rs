use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

const MAX_HEADER_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DagSessionStartEvent {
    pub reason: String,
    pub previous_session_file: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DagSessionHeader {
    pub id: String,
    pub parent_session: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DagForkSourceResolution {
    Source { session_id: String },
    OwnOnly { diagnostic: Option<String> },
}

impl DagForkSourceResolution {
    pub fn session_id(&self) -> Option<&str> {
        match self {
            Self::Source { session_id } => Some(session_id),
            Self::OwnOnly { .. } => None,
        }
    }

    pub fn diagnostic(&self) -> Option<&str> {
        match self {
            Self::Source { .. } => None,
            Self::OwnOnly { diagnostic } => diagnostic.as_deref(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolveDagForkSourceInput {
    pub event: serde_json::Value,
    pub current_session_id: String,
    pub current_session_file: Option<String>,
}

fn non_empty_string(value: Option<&serde_json::Value>) -> Option<String> {
    value
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
}

pub fn parse_dag_session_start_event(event: &serde_json::Value) -> Option<DagSessionStartEvent> {
    let object = event.as_object()?;
    let reason = non_empty_string(object.get("reason"))?;
    Some(DagSessionStartEvent {
        reason,
        previous_session_file: non_empty_string(object.get("previousSessionFile")),
    })
}

pub fn read_dag_session_header(session_file: impl AsRef<Path>) -> Result<DagSessionHeader, String> {
    let mut file = File::open(session_file).map_err(|error| error.to_string())?;
    let metadata = file.metadata().map_err(|error| error.to_string())?;
    if !metadata.is_file() {
        return Err("session file is not a regular file".into());
    }
    let mut bytes = Vec::with_capacity(MAX_HEADER_BYTES + 1);
    let mut byte = [0_u8; 1];
    while bytes.len() <= MAX_HEADER_BYTES {
        match file.read(&mut byte) {
            Ok(0) => break,
            Ok(_) if byte[0] == b'\n' => break,
            Ok(_) => bytes.push(byte[0]),
            Err(error) => return Err(error.to_string()),
        }
    }
    if bytes.len() > MAX_HEADER_BYTES {
        return Err("session header exceeds the read limit".into());
    }
    let header: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    if header.get("type").and_then(serde_json::Value::as_str) != Some("session") {
        return Err("session file has no session header".into());
    }
    let id = non_empty_string(header.get("id"))
        .ok_or_else(|| "session header has no identity".to_owned())?;
    Ok(DagSessionHeader {
        id,
        parent_session: non_empty_string(header.get("parentSession")),
    })
}

fn canonical_session_path(path: &str) -> PathBuf {
    let path = PathBuf::from(path);
    let absolute = if path.is_absolute() {
        path
    } else {
        std::env::current_dir().unwrap_or_default().join(path)
    };
    std::fs::canonicalize(&absolute).unwrap_or(absolute)
}

pub fn resolve_dag_fork_source(input: ResolveDagForkSourceInput) -> DagForkSourceResolution {
    let Some(event) = parse_dag_session_start_event(&input.event) else {
        return DagForkSourceResolution::OwnOnly { diagnostic: None };
    };
    if event.reason != "fork" {
        return DagForkSourceResolution::OwnOnly { diagnostic: None };
    }
    let Some(current_file) = input.current_session_file else {
        return DagForkSourceResolution::OwnOnly {
            diagnostic: Some("fork session has no session file to declare its parent".into()),
        };
    };
    let declared_parent = match read_dag_session_header(&current_file) {
        Ok(header) => match header.parent_session {
            Some(parent) => parent,
            None => {
                return DagForkSourceResolution::OwnOnly {
                    diagnostic: Some("fork session header declares no parent session".into()),
                };
            }
        },
        Err(error) => {
            return DagForkSourceResolution::OwnOnly {
                diagnostic: Some(format!("fork session header unreadable: {error}")),
            };
        }
    };
    if let Some(offered) = event.previous_session_file
        && canonical_session_path(&declared_parent) != canonical_session_path(&offered)
    {
        return DagForkSourceResolution::OwnOnly {
            diagnostic: Some(
                "fork event source does not match the session's declared parent".into(),
            ),
        };
    }
    match read_dag_session_header(&declared_parent) {
        Ok(source) if source.id == input.current_session_id => DagForkSourceResolution::OwnOnly {
            diagnostic: Some("fork source header carries the forked session's own identity".into()),
        },
        Ok(source) => DagForkSourceResolution::Source {
            session_id: source.id,
        },
        Err(error) => DagForkSourceResolution::OwnOnly {
            diagnostic: Some(format!("fork source unreadable: {error}")),
        },
    }
}

#[cfg(test)]
#[path = "dag_fork_source_tests.rs"]
mod tests;
