//! Port of senpi `packages/coding-agent/src/modes/rpc/session-move-request.ts`.
//!
//! Parses a `move_session` request into one op with its typed fields, or the refusal string.

use crate::session_move_wire::{MOVE_INLINE_MAX_BYTES, MOVE_INLINE_MAX_FILES};
use maho_core::session_move::bundle::{
    RepositoryIdentity, is_plain_id, is_session_file_name, parse_move_id, parse_repository_identity,
};
use maho_core::session_move::inline_bundle::InlineBundleFile;
use maho_core::session_move::move_ledger::{MoveAnswer, parse_move_status};
use serde_json::{Map, Value};
use std::path::Path;

const PLAIN_ID_RULE: &str =
    "a letter or digit, then letters, digits, '.', '_' or '-', at most 128 characters";
const OPEN_FIELDS_THE_IMPORT_SETS: [&str; 5] = ["id", "type", "sessionPath", "cwd", "admissionId"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StatusNamedFile {
    Path {
        session_path: String,
    },
    Landing {
        cwd: String,
        session_file_name: String,
        session_dir: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum ImportBundle {
    Dir(String),
    Files(Vec<InlineBundleFile>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum MoveRequest {
    Prepare {
        move_id: String,
        cwd: Option<String>,
        repository: Option<RepositoryIdentity>,
    },
    Cancel {
        move_id: String,
    },
    IdentitySession {
        session_path: String,
    },
    IdentityCwd {
        cwd: String,
    },
    Status {
        move_id: String,
        named: Option<StatusNamedFile>,
    },
    Export {
        session_path: String,
        move_id: String,
        target: String,
        dropped: Vec<String>,
        inline_max_bytes: Option<usize>,
    },
    Import {
        bundle: ImportBundle,
        cwd: String,
        from: String,
        session_dir: Option<String>,
    },
    Finish {
        session_path: String,
        move_id: String,
        answer: MoveAnswer,
    },
    Revoke {
        session_path: String,
        move_id: String,
    },
    Copy {
        session_path: String,
        session_id: String,
        session_dir: Option<String>,
    },
}

fn non_empty_text(value: Option<&Value>) -> Option<&str> {
    value
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
}

fn plain_move_id(value: Option<&Value>) -> Option<String> {
    value.and_then(|v| parse_move_id(v).ok())
}

fn well_shaped_repository(value: &Value) -> bool {
    let Some(obj) = value.as_object() else {
        return false;
    };
    let Some(commits) = obj.get("rootCommits").and_then(Value::as_array) else {
        return false;
    };
    if !commits.iter().all(Value::is_string) {
        return false;
    }
    obj.get("originUrl").is_none_or(Value::is_string)
}

fn is_padded_base64(value: &str) -> bool {
    if !value.len().is_multiple_of(4) {
        return false;
    }
    let bytes = value.as_bytes();
    let pad = bytes
        .iter()
        .rev()
        .take(2)
        .take_while(|&&b| b == b'=')
        .count();
    let body = &bytes[..bytes.len() - pad];
    body.iter()
        .all(|b| b.is_ascii_alphanumeric() || *b == b'+' || *b == b'/')
}

fn base64_decoded_len(encoded: &str) -> usize {
    let pad = encoded
        .as_bytes()
        .iter()
        .rev()
        .take(2)
        .take_while(|&&b| b == b'=')
        .count();
    (encoded.len() / 4) * 3 - pad
}

fn parse_inline_files(value: &Value) -> Result<Vec<InlineBundleFile>, String> {
    let Some(arr) = value.as_array() else {
        return Err(format!(
            "files must be a list of at most {MOVE_INLINE_MAX_FILES} {{ path, sha256, base64 }} entries."
        ));
    };
    if arr.len() > MOVE_INLINE_MAX_FILES {
        return Err(format!(
            "files must be a list of at most {MOVE_INLINE_MAX_FILES} {{ path, sha256, base64 }} entries."
        ));
    }
    let mut files = Vec::with_capacity(arr.len());
    let mut bytes = 0usize;
    for item in arr {
        let Some(obj) = item.as_object() else {
            return Err("each file must be an object with path, sha256 and base64.".to_owned());
        };
        let (Some(path), Some(sha256), Some(b64)) = (
            obj.get("path").and_then(Value::as_str),
            obj.get("sha256").and_then(Value::as_str),
            obj.get("base64").and_then(Value::as_str),
        ) else {
            return Err(
                "each file needs path and sha256 as strings and base64 as padded base64."
                    .to_owned(),
            );
        };
        if !is_padded_base64(b64) {
            return Err(
                "each file needs path and sha256 as strings and base64 as padded base64."
                    .to_owned(),
            );
        }
        bytes = bytes.saturating_add(base64_decoded_len(b64));
        if bytes > MOVE_INLINE_MAX_BYTES {
            return Err(format!(
                "files hold more than the {MOVE_INLINE_MAX_BYTES} bytes an inline import takes; send the bundle folder instead."
            ));
        }
        files.push(InlineBundleFile {
            path: path.to_owned(),
            sha256: sha256.to_owned(),
            base64: b64.to_owned(),
        });
    }
    Ok(files)
}

fn parse_identity(raw: &Map<String, Value>) -> Result<MoveRequest, String> {
    let session_path = non_empty_text(raw.get("sessionPath")).map(str::to_owned);
    let cwd = non_empty_text(raw.get("cwd")).map(str::to_owned);
    if session_path.is_some() && cwd.is_some() {
        return Err(
            "identity takes sessionPath, a session file of this device, or cwd, a folder, never both."
                .to_owned(),
        );
    }
    if let Some(session_path) = session_path {
        return Ok(MoveRequest::IdentitySession { session_path });
    }
    let Some(cwd) = cwd else {
        return Err(
            "identity needs sessionPath, a session file of this device, or cwd, a folder."
                .to_owned(),
        );
    };
    if !Path::new(&cwd).is_absolute() {
        return Err("identity cwd must be an absolute path.".to_owned());
    }
    Ok(MoveRequest::IdentityCwd { cwd })
}

fn parse_prepare(raw: &Map<String, Value>, move_id: String) -> Result<MoveRequest, String> {
    let cwd = non_empty_text(raw.get("cwd")).map(str::to_owned);
    if raw.get("cwd").is_some_and(|v| !v.is_null()) && cwd.is_none() {
        return Err("prepare cwd must be a non-empty string.".to_owned());
    }
    if let Some(ref cwd_str) = cwd
        && !Path::new(cwd_str).is_absolute()
    {
        return Err("prepare cwd must be an absolute path.".to_owned());
    }
    let repo_val = raw.get("repository").filter(|v| !v.is_null());
    if repo_val.is_some() && cwd.is_none() {
        return Err(
            "prepare needs cwd, the folder the move lands in, together with repository.".to_owned(),
        );
    }
    let Some(repo_val) = repo_val else {
        return Ok(MoveRequest::Prepare {
            move_id,
            cwd,
            repository: None,
        });
    };
    if !well_shaped_repository(repo_val) {
        return Err(
            "prepare repository must be { rootCommits: string[], originUrl?: string }, as the identity op answers it."
                .to_owned(),
        );
    }
    Ok(MoveRequest::Prepare {
        move_id,
        cwd,
        repository: parse_repository_identity(repo_val),
    })
}

fn parse_copy(raw: &Map<String, Value>) -> Result<MoveRequest, String> {
    let Some(session_path) = non_empty_text(raw.get("sessionPath")).map(str::to_owned) else {
        return Err("copy needs sessionPath, the session file to copy.".to_owned());
    };
    if !raw.get("sessionId").is_some_and(is_plain_id) {
        return Err(format!("sessionId must be a plain id: {PLAIN_ID_RULE}."));
    }
    let session_id = raw["sessionId"].as_str().unwrap_or_default().to_owned();
    let session_dir = match raw.get("sessionDir").filter(|v| !v.is_null()) {
        None => None,
        Some(v) => {
            let dir = non_empty_text(Some(v))
                .filter(|s| Path::new(s).is_absolute())
                .ok_or_else(|| "sessionDir must be an absolute path.".to_owned())?;
            Some(dir.to_owned())
        }
    };
    Ok(MoveRequest::Copy {
        session_path,
        session_id,
        session_dir,
    })
}

fn parse_status_file(raw: &Map<String, Value>) -> Result<Option<StatusNamedFile>, String> {
    let session_path = raw.get("sessionPath").filter(|v| !v.is_null());
    let landing = raw.get("landing").filter(|v| !v.is_null());
    let Some(landing) = landing else {
        let Some(sp) = session_path else {
            return Ok(None);
        };
        return non_empty_text(Some(sp))
            .map(|s| {
                Some(StatusNamedFile::Path {
                    session_path: s.to_owned(),
                })
            })
            .ok_or_else(|| "sessionPath must be a non-empty string.".to_owned());
    };
    if session_path.is_some() {
        return Err("status takes sessionPath or landing, not both.".to_owned());
    }
    let Some(obj) = landing.as_object() else {
        return Err(
            "landing must be an object: cwd, sessionFileName and, if the import named one, sessionDir."
                .to_owned(),
        );
    };
    let cwd = non_empty_text(obj.get("cwd"))
        .filter(|s| Path::new(s).is_absolute())
        .ok_or_else(|| "landing.cwd must be the absolute folder the import was given.".to_owned())?
        .to_owned();
    let file_name = obj
        .get("sessionFileName")
        .and_then(Value::as_str)
        .filter(|s| is_session_file_name(s))
        .ok_or_else(|| {
            "landing.sessionFileName must be the session's plain file name, as the bundle's manifest names it."
                .to_owned()
        })?
        .to_owned();
    let session_dir = match obj.get("sessionDir").filter(|v| !v.is_null()) {
        None => None,
        Some(v) => {
            let dir = non_empty_text(Some(v))
                .filter(|s| Path::new(s).is_absolute())
                .ok_or_else(|| "landing.sessionDir must be an absolute path.".to_owned())?;
            Some(dir.to_owned())
        }
    };
    Ok(Some(StatusNamedFile::Landing {
        cwd,
        session_file_name: file_name,
        session_dir,
    }))
}

fn parse_import(raw: &Map<String, Value>) -> Result<MoveRequest, String> {
    let cwd = non_empty_text(raw.get("cwd"))
        .filter(|s| Path::new(s).is_absolute())
        .map(str::to_owned);
    let bundle = match (
        raw.get("files").filter(|v| !v.is_null()),
        raw.get("bundleDir").filter(|v| !v.is_null()),
    ) {
        (None, dir) => {
            let dir_str = non_empty_text(dir)
                .filter(|s| Path::new(s).is_absolute())
                .ok_or_else(|| "import needs bundleDir and cwd as absolute paths.".to_owned())?;
            if cwd.is_none() {
                return Err("import needs bundleDir and cwd as absolute paths.".to_owned());
            }
            ImportBundle::Dir(dir_str.to_owned())
        }
        (Some(_), Some(_)) => return Err("import takes bundleDir or files, not both.".to_owned()),
        (Some(files), None) => ImportBundle::Files(parse_inline_files(files)?),
    };
    let cwd = cwd.ok_or_else(|| "import with files needs cwd as an absolute path.".to_owned())?;
    let from = match raw.get("from").filter(|v| !v.is_null()) {
        None => String::new(),
        Some(v) => v
            .as_str()
            .ok_or_else(|| "from must be a string.".to_owned())?
            .to_owned(),
    };
    let session_dir = match raw.get("sessionDir").filter(|v| !v.is_null()) {
        None => None,
        Some(v) => {
            let dir = non_empty_text(Some(v))
                .filter(|s| Path::new(s).is_absolute())
                .ok_or_else(|| "sessionDir must be an absolute path.".to_owned())?;
            Some(dir.to_owned())
        }
    };
    if let Some(open) = raw.get("open").filter(|v| !v.is_null()) {
        let valid = open.as_bool() == Some(true)
            || open.as_object().is_some_and(|obj| {
                OPEN_FIELDS_THE_IMPORT_SETS
                    .iter()
                    .all(|f| !obj.contains_key(*f))
            });
        if !valid {
            return Err(
                "open must be true or open_session fields other than id, type, sessionPath, cwd and admissionId."
                    .to_owned(),
            );
        }
    }
    Ok(MoveRequest::Import {
        bundle,
        cwd,
        from,
        session_dir,
    })
}

pub fn parse_move_request(raw: &Map<String, Value>) -> Result<MoveRequest, String> {
    let op = raw.get("op").and_then(Value::as_str).unwrap_or_default();
    match op {
        "import" => return parse_import(raw),
        "copy" => return parse_copy(raw),
        "identity" => return parse_identity(raw),
        _ => {}
    }
    let move_id = plain_move_id(raw.get("moveId"))
        .ok_or_else(|| format!("moveId must be a plain id: {PLAIN_ID_RULE}."))?;
    match op {
        "prepare" => parse_prepare(raw, move_id),
        "cancel" => Ok(MoveRequest::Cancel { move_id }),
        "status" => Ok(MoveRequest::Status {
            move_id,
            named: parse_status_file(raw)?,
        }),
        "export" => {
            let session_path = non_empty_text(raw.get("sessionPath"))
                .ok_or_else(|| "export needs sessionPath and target.".to_owned())?
                .to_owned();
            let target = non_empty_text(raw.get("target"))
                .ok_or_else(|| "export needs sessionPath and target.".to_owned())?
                .to_owned();
            let dropped = match raw.get("droppedUserMessages").filter(|v| !v.is_null()) {
                None => Vec::new(),
                Some(v) => v
                    .as_array()
                    .and_then(|arr| {
                        arr.iter()
                            .map(|item| item.as_str().map(str::to_owned))
                            .collect::<Option<Vec<_>>>()
                    })
                    .ok_or_else(|| {
                        "droppedUserMessages must be the dropped.user_messages strings release_session returned."
                            .to_owned()
                    })?,
            };
            let inline_flag = raw.get("inline").filter(|v| !v.is_null());
            if inline_flag.is_some_and(|v| v.as_bool() != Some(true)) {
                return Err("inline must be true or omitted.".to_owned());
            }
            let cap = match raw.get("inlineMaxBytes").filter(|v| !v.is_null()) {
                None => MOVE_INLINE_MAX_BYTES,
                Some(v) => v
                    .as_u64()
                    .and_then(|n| usize::try_from(n).ok())
                    .ok_or_else(|| "inlineMaxBytes must be a non-negative integer.".to_owned())?,
            };
            let inline_max_bytes = if inline_flag.is_some() {
                Some(cap.min(MOVE_INLINE_MAX_BYTES))
            } else {
                None
            };
            Ok(MoveRequest::Export {
                session_path,
                move_id,
                target,
                dropped,
                inline_max_bytes,
            })
        }
        "finish" => {
            let session_path = non_empty_text(raw.get("sessionPath"))
                .and_then(|sp| {
                    raw.get("answer")
                        .and_then(parse_move_status)
                        .map(|ans| (sp.to_owned(), ans))
                })
                .ok_or_else(|| {
                    "finish needs sessionPath and the receiver's answer from status (receiving, accepted, declined or failed)."
                        .to_owned()
                })?;
            Ok(MoveRequest::Finish {
                session_path: session_path.0,
                move_id,
                answer: session_path.1,
            })
        }
        "revoke" => {
            let session_path = non_empty_text(raw.get("sessionPath"))
                .ok_or_else(|| {
                    "revoke needs sessionPath, the copy this host imported for the move.".to_owned()
                })?
                .to_owned();
            Ok(MoveRequest::Revoke {
                session_path,
                move_id,
            })
        }
        _ => Err(
            "op must be one of \"prepare\", \"cancel\", \"status\", \"export\", \"import\", \"finish\", \"revoke\", \"copy\" or \"identity\"."
                .to_owned(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_new_ops_and_rejects_bad_inputs() {
        let copy = json!({"op":"copy","sessionPath":"/a.jsonl","sessionId":"copy-1"});
        assert!(matches!(
            parse_move_request(copy.as_object().expect("obj")),
            Ok(MoveRequest::Copy { .. })
        ));
        let identity = json!({"op":"identity","cwd":"/repo"});
        assert!(matches!(
            parse_move_request(identity.as_object().expect("obj")),
            Ok(MoveRequest::IdentityCwd { .. })
        ));
        let bad_open =
            json!({"op":"import","bundleDir":"/b","cwd":"/c","open":{"cwd":"/override"}});
        assert!(parse_move_request(bad_open.as_object().expect("obj")).is_err());
    }
}
