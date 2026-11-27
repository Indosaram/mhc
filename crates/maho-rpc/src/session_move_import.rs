//! Port of senpi `packages/coding-agent/src/modes/rpc/session-move-import.ts`.
//!
//! `move_session` op `import` (senpi#3676, senpi#6267).

use crate::session_move_request::{ImportBundle, MoveRequest};
use maho_core::session_manager::{get_default_session_dir_with, load_entries_from_file};
use maho_core::session_move::import::{ImportMovedSessionOptions, import_moved_session};
use maho_core::session_move::inline_bundle::with_staged_inline_bundle;
use maho_core::session_move::move_ledger::MoveLedger;
use maho_core::session_move::ownership::header_session_id;
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};

#[async_trait::async_trait]
pub trait MoveImportPort: Send + Sync {
    fn agent_dir(&self) -> &Path;
    async fn open_landed(&self, open_command: Value) -> Result<Option<Value>, Value>;
}

pub async fn session_dir_refusal(session_dir: Option<&str>) -> Option<String> {
    let dir = session_dir?;
    match tokio::fs::metadata(dir).await {
        Ok(meta) if meta.is_dir() => {
            let probe_ok = tokio::fs::read_dir(dir).await.is_ok();
            if probe_ok {
                None
            } else {
                Some("sessionDir cannot be used (EACCES): this host cannot write there.".to_owned())
            }
        }
        Ok(_) => {
            Some("sessionDir must be a folder: a file is in its place or above it.".to_owned())
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
        Err(err) if err.kind() == std::io::ErrorKind::NotADirectory => {
            Some("sessionDir must be a folder: a file is in its place or above it.".to_owned())
        }
        Err(err) => Some(format!(
            "sessionDir cannot be used ({}): this host could not look at it.",
            err
        )),
    }
}

pub async fn import_moved(
    parsed: &MoveRequest,
    agent_dir: &Path,
    ledger: &MoveLedger,
    root: &Path,
) -> Result<(String, String), String> {
    let MoveRequest::Import {
        bundle,
        cwd,
        from,
        session_dir,
    } = parsed
    else {
        return Err("expected import request".to_owned());
    };
    let resolved_dir = match session_dir {
        Some(dir) => PathBuf::from(dir),
        None => PathBuf::from(get_default_session_dir_with(
            cwd,
            &agent_dir.to_string_lossy(),
        )),
    };
    let target_cwd = Path::new(cwd);
    let session_file = match bundle {
        ImportBundle::Dir(dir) => {
            import_moved_session(ImportMovedSessionOptions {
                bundle_dir: Path::new(dir),
                target_cwd,
                ledger,
                session_dir: Some(&resolved_dir),
                from: (!from.is_empty()).then_some(from.as_str()),
                read_identity: None,
            })
            .await?
        }
        ImportBundle::Files(files) => {
            let incoming = root.join("incoming");
            with_staged_inline_bundle(
                files,
                &incoming,
                |bundle_dir, _sent| {
                    let resolved_dir = resolved_dir.clone();
                    let from = from.clone();
                    async move {
                        import_moved_session(ImportMovedSessionOptions {
                            bundle_dir: &bundle_dir,
                            target_cwd,
                            ledger,
                            session_dir: Some(&resolved_dir),
                            from: (!from.is_empty()).then_some(from.as_str()),
                            read_identity: None,
                        })
                        .await
                    }
                },
                None,
            )
            .await?
        }
    };
    let sf_str = session_file.to_string_lossy().into_owned();
    let entries = tokio::task::spawn_blocking(move || load_entries_from_file(&sf_str))
        .await
        .map_err(|e| e.to_string())?;
    let session_id = header_session_id(&entries).ok_or_else(|| {
        format!(
            "The imported session at {} has no id in its header.",
            session_file.display()
        )
    })?;
    Ok((
        session_id.to_owned(),
        session_file.to_string_lossy().into_owned(),
    ))
}

pub async fn open_landed(
    port: &dyn MoveImportPort,
    raw_command: &Map<String, Value>,
    cwd: &str,
    session_path: &str,
) -> Option<Value> {
    let open = raw_command.get("open").filter(|v| !v.is_null())?;
    let mut open_cmd = match open {
        Value::Bool(true) => Map::new(),
        Value::Object(map) => map.clone(),
        _ => return None,
    };
    if let Some(id) = raw_command.get("id") {
        open_cmd.insert("id".to_owned(), id.clone());
    }
    open_cmd.insert("type".to_owned(), json!("open_session"));
    open_cmd.insert("sessionPath".to_owned(), json!(session_path));
    open_cmd.insert("cwd".to_owned(), json!(cwd));
    match port.open_landed(Value::Object(open_cmd)).await {
        Ok(None) => None,
        Ok(Some(err)) | Err(err) => Some(json!({
            "error": err.get("error").and_then(Value::as_str).unwrap_or("open failed"),
            "errorCode": err.get("errorCode").cloned().unwrap_or(Value::Null),
            "errorData": err.get("errorData").cloned().unwrap_or(Value::Null),
        })),
    }
}
