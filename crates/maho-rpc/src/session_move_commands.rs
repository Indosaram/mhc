//! RPC move transaction dispatch (senpi `modes/rpc/session-move-commands.ts`).
//! File custody stays in `maho_core::session_move`.

use crate::session_move_import::{MoveImportPort, import_moved, open_landed, session_dir_refusal};
use crate::session_move_landed::{landed_when_fenced, status_answer};
use crate::session_move_request::{MoveRequest, parse_move_request};
use crate::session_move_wire::MOVE_INLINE_MAX_FILES;
use async_trait::async_trait;
use maho_core::repository_identity::read_repository_identity;
use maho_core::session_manager::{get_default_session_dir_with, load_entries_from_file};
use maho_core::session_move::{
    copy::copy_session_as,
    export::{ExportSessionForMoveOptions, export_session_for_move_and_read},
    extension_state::sealed_outgoing_state_dir,
    finish::{FinishMoveOptions, finish_move},
    identity::read_session_repository_identity,
    inline_bundle::{InlineBundleLimits, read_inline_bundle},
    move_ledger::{DeclineReason, MoveFencedError, MoveLedger, MoveRecord, MoveStatus},
    ownership::{RefusedState, SessionOwnership, session_ownership},
    prepare::{PrepareMoveOptions, prepare_move},
    revoke::{RevokeMoveOptions, revoke_imported_move},
};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

#[async_trait]
pub trait MoveSessionPort: Send + Sync {
    fn agent_dir(&self) -> &Path;
    fn hosted_paths(&self) -> Vec<String>;
    async fn open_landed(&self, open_command: Value) -> Result<Option<Value>, Value>;
}

struct SimpleMovePort<'a> {
    agent_dir: &'a Path,
    hosted_paths: &'a [String],
}

#[async_trait]
impl MoveSessionPort for SimpleMovePort<'_> {
    fn agent_dir(&self) -> &Path {
        self.agent_dir
    }
    fn hosted_paths(&self) -> Vec<String> {
        self.hosted_paths.to_vec()
    }
    async fn open_landed(&self, _open_command: Value) -> Result<Option<Value>, Value> {
        Ok(Some(json!({
            "error": "open_unsupported",
            "errorCode": "open_unsupported",
        })))
    }
}

struct PortImportAdapter<'a> {
    port: &'a dyn MoveSessionPort,
}

#[async_trait]
impl MoveImportPort for PortImportAdapter<'_> {
    fn agent_dir(&self) -> &Path {
        self.port.agent_dir()
    }
    async fn open_landed(&self, open_command: Value) -> Result<Option<Value>, Value> {
        self.port.open_landed(open_command).await
    }
}

fn wire_record(record: MoveRecord) -> Value {
    match record {
        MoveRecord::Ready { expires_at } => json!({"state": "ready", "expires_at": expires_at}),
        MoveRecord::Status(status) => crate::session_move_landed::move_status_to_value(&status),
    }
}

fn refusal(id: Option<&Value>, code: &str, detail: &str, landed: Option<Value>) -> Value {
    let mut error_data = json!({ "detail": detail });
    if let Some(Value::Object(map)) = landed
        && let Some(obj) = error_data.as_object_mut()
    {
        for (k, v) in map {
            obj.insert(k, v);
        }
    }
    json!({
        "id": id,
        "type": "response",
        "command": "move_session",
        "success": false,
        "error": code,
        "errorCode": code,
        "errorData": error_data,
    })
}

pub fn validate_move_request(command: &Value) -> Result<(), String> {
    let Some(obj) = command.as_object() else {
        return Err("move_session request must be an object".to_owned());
    };
    parse_move_request(obj).map(|_| ())
}

pub async fn move_session(agent_dir: &str, hosted_paths: &[String], command: &Value) -> Value {
    let port = SimpleMovePort {
        agent_dir: Path::new(agent_dir),
        hosted_paths,
    };
    move_session_with_port(&port, command).await
}

pub async fn move_session_with_port(port: &dyn MoveSessionPort, command: &Value) -> Value {
    let id_val = command.get("id");
    let Some(obj) = command.as_object() else {
        return refusal(
            id_val,
            "invalid_move_request",
            "move_session request must be an object",
            None,
        );
    };
    let parsed = match parse_move_request(obj) {
        Ok(p) => p,
        Err(err) => return refusal(id_val, "invalid_move_request", &err, None),
    };
    let agent_dir = port.agent_dir();
    let root = agent_dir.join("session-moves");
    let ledger = MoveLedger::new(root.join("ledger"));

    if let MoveRequest::Import {
        ref session_dir, ..
    } = parsed
    {
        if let Some(detail) = session_dir_refusal(session_dir.as_deref()).await {
            return refusal(id_val, "invalid_move_request", &detail, None);
        }
        let adapter = PortImportAdapter { port };
        match import_moved(&parsed, agent_dir, &ledger, &root).await {
            Ok((session_id, session_path)) => {
                let cwd = match &parsed {
                    MoveRequest::Import { cwd, .. } => cwd.as_str(),
                    _ => "",
                };
                if let Some(open_err) = open_landed(&adapter, obj, cwd, &session_path).await {
                    return json!({
                        "id": id_val,
                        "type": "response",
                        "command": "move_session",
                        "success": true,
                        "data": {
                            "session_id": session_id,
                            "session_path": session_path,
                            "open_error": open_err,
                        },
                    });
                }
                return json!({
                    "id": id_val,
                    "type": "response",
                    "command": "move_session",
                    "success": true,
                    "data": {
                        "session_id": session_id,
                        "session_path": session_path,
                    },
                });
            }
            Err(err) => {
                let code = map_move_error_code(&err);
                return refusal(id_val, code, &err, None);
            }
        }
    }

    let hosted = port.hosted_paths();
    let move_id_for_fence = match &parsed {
        MoveRequest::Prepare { move_id, .. }
        | MoveRequest::Cancel { move_id }
        | MoveRequest::Status { move_id, .. }
        | MoveRequest::Export { move_id, .. }
        | MoveRequest::Finish { move_id, .. }
        | MoveRequest::Revoke { move_id, .. } => Some(move_id.clone()),
        _ => None,
    };

    match run_non_import(parsed, port, &hosted, &ledger, &root).await {
        Ok(data) => json!({
            "id": id_val,
            "type": "response",
            "command": "move_session",
            "success": true,
            "data": data,
        }),
        Err(err) => {
            let code = map_move_error_code(&err);
            let landed = if code == "move_fenced"
                && let Some(mid) = move_id_for_fence
            {
                let record = ledger
                    .answer_status(&mid, false)
                    .await
                    .map(MoveRecord::Status)
                    .unwrap_or(MoveRecord::Status(MoveStatus::Accepted));
                let fenced = MoveFencedError::new(mid, record, None);
                landed_when_fenced(&ledger, &fenced).await
            } else {
                None
            };
            refusal(id_val, code, &err, landed)
        }
    }
}

fn map_move_error_code(error: &str) -> &'static str {
    let head = error.split(':').next().unwrap_or_default().trim();
    match head {
        "invalid_move_request" => "invalid_move_request",
        "session_open_here" => "session_open_here",
        "move_not_released" => "move_not_released",
        "move_fenced" => "move_fenced",
        "move_bundle_invalid" => "move_bundle_invalid",
        "move_repository_mismatch" => "move_repository_mismatch",
        "workspace_missing" => "workspace_missing",
        "move_target_occupied" => "move_target_occupied",
        "move_mismatch" => "move_mismatch",
        "move_already_moved" => "move_already_moved",
        "session_moved_away" => "session_moved_away",
        "ESESSIONHELD" => "session_held",
        _ => "move_failed",
    }
}

async fn run_non_import(
    parsed: MoveRequest,
    port: &dyn MoveSessionPort,
    hosted_paths: &[String],
    ledger: &MoveLedger,
    root: &Path,
) -> Result<Value, String> {
    let agent_dir = port.agent_dir();
    match parsed {
        MoveRequest::Prepare {
            move_id,
            cwd,
            repository,
        } => {
            let record = prepare_move(PrepareMoveOptions {
                ledger,
                move_id: &move_id,
                cwd: cwd.as_deref(),
                repository: repository.as_ref(),
            })
            .await?;
            Ok(wire_record(record))
        }
        MoveRequest::IdentityCwd { cwd } => {
            let resolved = tokio::fs::canonicalize(&cwd)
                .await
                .unwrap_or_else(|_| PathBuf::from(&cwd));
            let repo = read_repository_identity(&resolved.to_string_lossy()).await;
            Ok(match repo {
                None => json!({}),
                Some(r) => json!({ "repository": r }),
            })
        }
        MoveRequest::IdentitySession { session_path } => {
            let sessions_root = agent_dir.join("sessions");
            let repo = read_session_repository_identity(&sessions_root, Path::new(&session_path))
                .await
                .map_err(|e| e.to_string())?;
            Ok(match repo {
                None => json!({}),
                Some(r) => json!({ "repository": r }),
            })
        }
        MoveRequest::Cancel { move_id } => {
            let record = ledger
                .decline(&move_id, DeclineReason::Cancelled)
                .await
                .map_err(|e| e.to_string())?;
            Ok(wire_record(record))
        }
        MoveRequest::Status { move_id, named } => {
            status_answer(ledger, &move_id, named.as_ref(), agent_dir).await
        }
        MoveRequest::Export {
            session_path,
            move_id,
            target,
            dropped,
            inline_max_bytes,
        } => {
            let path = ordered_real_path(&session_path).await;
            closed(hosted_paths, &path.to_string_lossy())?;
            let path_str = path.to_string_lossy().into_owned();
            let entries = tokio::task::spawn_blocking(move || load_entries_from_file(&path_str))
                .await
                .map_err(|e| e.to_string())?;
            if !matches!(
                session_ownership(&entries),
                SessionOwnership::Refused(ref owner)
                    if owner.state == RefusedState::Moving && owner.move_id == move_id
            ) {
                return Err("move_not_released: release the session for this move first".to_owned());
            }
            let destination = root.join("outgoing").join(&move_id);
            let state = sealed_outgoing_state_dir(&path, &move_id).await?;
            let guard = || closed(hosted_paths, &path.to_string_lossy());
            let (manifest, inline) = export_session_for_move_and_read(
                ExportSessionForMoveOptions {
                    session_file: &path,
                    move_id: &move_id,
                    target: &target,
                    dropped_user_messages: &dropped,
                    bundle_dir: &destination,
                    extension_state_dir: state.as_deref(),
                    resolve_extension_state: None,
                    guard: Some(&guard),
                    locked_guard: Some(&guard),
                },
                |written| {
                    let written = written.to_path_buf();
                    async move {
                        let Some(cap) = inline_max_bytes else {
                            return Ok(None);
                        };
                        read_inline_bundle(
                            &written,
                            InlineBundleLimits {
                                max_bytes: cap as u64,
                                max_files: MOVE_INLINE_MAX_FILES,
                            },
                        )
                        .await
                    }
                },
            )
            .await?;
            let files = 1
                + manifest.sidecars.len()
                + manifest
                    .payloads
                    .iter()
                    .map(|p| p.files.len())
                    .sum::<usize>();
            let mut out = json!({
                "move_id": manifest.move_id,
                "session_id": manifest.session_id,
                "bundle_dir": destination,
                "files": files,
            });
            if let Some(inline_files) = inline {
                out["inline_files"] = serde_json::to_value(inline_files).unwrap_or(Value::Null);
            }
            Ok(out)
        }
        MoveRequest::Finish {
            session_path,
            move_id,
            answer,
        } => {
            let path = ordered_real_path(&session_path).await;
            let guard = || closed(hosted_paths, &path.to_string_lossy());
            finish_move(FinishMoveOptions {
                session_file: &path,
                bundle_dir: &root.join("outgoing").join(&move_id),
                move_id: &move_id,
                answer: &answer,
                guard: Some(&guard),
            })
            .await?;
            let path_str = path.to_string_lossy().into_owned();
            let entries = tokio::task::spawn_blocking(move || load_entries_from_file(&path_str))
                .await
                .map_err(|e| e.to_string())?;
            let state = match session_ownership(&entries) {
                SessionOwnership::Writable => "writable",
                SessionOwnership::Refused(owner) => {
                    if owner.state == RefusedState::Moved {
                        "moved"
                    } else {
                        "moving"
                    }
                }
            };
            Ok(json!({ "ownership": state }))
        }
        MoveRequest::Revoke {
            session_path,
            move_id,
        } => {
            let path = ordered_real_path(&session_path).await;
            let guard = || closed(hosted_paths, &path.to_string_lossy());
            revoke_imported_move(RevokeMoveOptions {
                session_file: &path,
                move_id: &move_id,
                ledger,
                guard: Some(&guard),
            })
            .await?;
            Ok(json!({ "ownership": "moved" }))
        }
        MoveRequest::Copy {
            session_path,
            session_id,
            session_dir,
        } => {
            let path = ordered_real_path(&session_path).await;
            let agent_dir_str = agent_dir.to_string_lossy().into_owned();
            let copied = copy_session_as(&path, &session_id, |cwd| {
                session_dir
                    .as_deref()
                    .map(str::to_owned)
                    .unwrap_or_else(|| get_default_session_dir_with(cwd, &agent_dir_str))
            })
            .await?;
            Ok(json!({
                "session_id": copied.session_id,
                "session_path": copied.session_path,
            }))
        }
        MoveRequest::Import { .. } => Err("unreachable import".to_owned()),
    }
}

async fn ordered_real_path(path: &str) -> PathBuf {
    static CHAINS: std::sync::OnceLock<
        std::sync::Mutex<
            std::collections::HashMap<PathBuf, std::sync::Arc<tokio::sync::Mutex<()>>>,
        >,
    > = std::sync::OnceLock::new();
    let key = std::path::absolute(path).unwrap_or_else(|_| PathBuf::from(path));
    let chain = std::sync::Arc::clone(
        CHAINS
            .get_or_init(Default::default)
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(key.clone())
            .or_default(),
    );
    let turn = chain.lock().await;
    let resolved = tokio::fs::canonicalize(path)
        .await
        .unwrap_or_else(|_| PathBuf::from(path));
    drop(turn);
    let mut chains = CHAINS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if chains.get(&key).is_some_and(|tail| {
        std::sync::Arc::ptr_eq(tail, &chain) && std::sync::Arc::strong_count(tail) == 2
    }) {
        chains.remove(&key);
    }
    resolved
}

fn real(path: &str) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| PathBuf::from(path))
}

fn closed(hosted: &[String], path: &str) -> Result<(), String> {
    if hosted.iter().any(|hosted| real(hosted) == real(path)) {
        Err("session_open_here: close the session here before moving it".into())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_invalid_move_answers_and_relative_import_paths() {
        for answer in [
            json!({"state":"ready"}),
            json!({"state":"declined","reason":"typo"}),
            json!({"state":"failed"}),
        ] {
            assert!(
                validate_move_request(
                    &json!({"op":"finish","moveId":"m","sessionPath":"/session","answer":answer})
                )
                .is_err()
            );
        }
        assert!(
            validate_move_request(&json!({"op":"import","bundleDir":"relative","cwd":"/repo"}))
                .is_err()
        );
        assert!(
            validate_move_request(&json!({"op":"finish","moveId":"m","sessionPath":"/session","answer":{"state":"accepted","revoked":true}}))
                .is_ok()
        );
        assert!(
            validate_move_request(
                &json!({"op":"copy","sessionPath":"/s.jsonl","sessionId":"copy1"})
            )
            .is_ok()
        );
    }
}
