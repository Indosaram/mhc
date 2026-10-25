//! RPC move transaction dispatch. File custody stays in the engine ledger.
use maho_core::session_move::{
    bundle,
    move_ledger::{DeclineReason, MoveLedger, MoveRecord, MoveStatus},
    ownership::{RefusedState, SessionOwnership, session_ownership},
};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
fn wire(record: MoveRecord) -> Value {
    match record {
        MoveRecord::Ready { expires_at } => json!({"state":"ready","expires_at":expires_at}),
        MoveRecord::Status(status) => status_wire(status),
    }
}
fn status_wire(status: MoveStatus) -> Value {
    match status {
        MoveStatus::Receiving => json!({"state":"receiving"}),
        MoveStatus::AcceptedRevoked => json!({"state":"accepted","revoked":true}),
        MoveStatus::Accepted => json!({"state":"accepted"}),
        MoveStatus::Declined { reason } => json!({"state":"declined","reason":reason.as_str()}),
        MoveStatus::Failed { reason } => json!({"state":"failed","reason":reason.as_str()}),
    }
}
fn refusal(command: &Value, code: &str, detail: &str) -> Value {
    json!({"id":command["id"],"type":"response","command":"move_session","success":false,"error":code,"errorCode":code,"errorData":{"detail":detail}})
}
pub fn validate_move_request(command: &Value) -> Result<(), String> {
    let op = command["op"].as_str().unwrap_or_default();
    if op == "import" {
        let dir = text(command, "bundleDir")?;
        let cwd = text(command, "cwd")?;
        if !Path::new(dir).is_absolute() || !Path::new(cwd).is_absolute() {
            return Err("import needs bundleDir and cwd as absolute paths.".into());
        }
        if command.get("from").is_some_and(|value| !value.is_string()) {
            return Err("from must be a string.".into());
        }
        return Ok(());
    }
    if !bundle::is_move_id(&command["moveId"]) {
        return Err("moveId must be a plain id.".into());
    }
    match op {
        "prepare" | "cancel" => {}
        "status" => {
            if command.get("sessionPath").is_some() {
                text(command, "sessionPath")?;
            }
        }
        "export" => {
            text(command, "sessionPath")?;
            text(command, "target")?;
            if command.get("droppedUserMessages").is_some_and(|value| {
                value
                    .as_array()
                    .is_none_or(|items| items.iter().any(|item| !item.is_string()))
            }) {
                return Err("droppedUserMessages must contain strings.".into());
            }
        }
        "finish" => {
            text(command, "sessionPath")?;
            let answer = &command["answer"];
            let valid = match answer["state"].as_str() {
                Some("receiving" | "accepted") => true,
                Some("declined") => matches!(
                    answer["reason"].as_str(),
                    Some(
                        "no_record"
                            | "expired"
                            | "cancelled"
                            | "chunk_without_ready"
                            | "repository"
                    )
                ),
                Some("failed") => matches!(
                    answer["reason"].as_str(),
                    Some("bundle_invalid" | "import_failed")
                ),
                _ => false,
            };
            if !valid {
                return Err("finish needs the receiver's answer from status.".into());
            }
        }
        "revoke" => {
            text(command, "sessionPath")?;
        }
        _ => return Err("invalid move operation".into()),
    }
    Ok(())
}
pub async fn move_session(agent_dir: &str, hosted_paths: &[String], command: &Value) -> Value {
    if let Err(error) = validate_move_request(command) {
        return refusal(command, "invalid_move_request", &error);
    }
    let op = command["op"].as_str().unwrap_or_default();
    if op != "import" && !bundle::is_move_id(&command["moveId"]) {
        return refusal(
            command,
            "invalid_move_request",
            "moveId must be a plain id: a letter or digit, then letters, digits, '.', '_' or '-', at most 128 characters.",
        );
    }
    let root = Path::new(agent_dir).join("session-moves");
    let ledger = MoveLedger::new(root.join("ledger"));
    let id = command["moveId"].as_str().unwrap_or_default();
    let result:Result<Value,String>=async {match op {
        "prepare"=>ledger.prepare(id).await.map(wire).map_err(|error|error.to_string()),
        "cancel"=>ledger.decline(id,DeclineReason::Cancelled).await.map(wire).map_err(|error|error.to_string()),
        "status"=>maho_core::session_move::import::answer_move_status(&ledger,id,command["sessionPath"].as_str().map(Path::new)).await.map(status_wire),
        "export"=>{
            let path=Path::new(text(command,"sessionPath")?);closed(hosted_paths,&path.to_string_lossy())?;
            let entries=maho_core::session_manager::load_entries_from_file(&path.to_string_lossy());
            if !matches!(session_ownership(&entries),SessionOwnership::Refused(ref owner) if owner.state==RefusedState::Moving&&owner.move_id==id){return Err("move_not_released: release the session for this move first".into());}
            let destination=root.join("outgoing").join(id);
            let dropped=command["droppedUserMessages"].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_owned).collect::<Vec<_>>();
            let state=maho_core::session_move::extension_state::sealed_outgoing_state_dir(path,id)?;
            let guard=||closed(hosted_paths,&path.to_string_lossy());
            let manifest=maho_core::session_move::export::export_session_for_move(maho_core::session_move::export::ExportSessionForMoveOptions{session_file:path,move_id:id,target:text(command,"target")?,dropped_user_messages:&dropped,bundle_dir:&destination,extension_state_dir:state.as_deref(),resolve_extension_state:None,guard:Some(&guard),locked_guard:Some(&guard)}).await?;
            let files=1+manifest.sidecars.len()+manifest.payloads.iter().map(|payload|payload.files.len()).sum::<usize>();
            Ok(json!({"move_id":id,"session_id":manifest.session_id,"bundle_dir":destination,"files":files}))
        },
        "finish"=>{
            let path=Path::new(text(command,"sessionPath")?);let guard=||closed(hosted_paths,&path.to_string_lossy());
            let answer=maho_core::session_move::move_ledger::parse_move_status(&command["answer"]).ok_or("invalid_move_request: invalid move answer")?;
            maho_core::session_move::finish::finish_move(maho_core::session_move::finish::FinishMoveOptions{session_file:path,bundle_dir:&root.join("outgoing").join(id),move_id:id,answer:&answer,guard:Some(&guard)}).await?;
            let entries=maho_core::session_manager::load_entries_from_file(&path.to_string_lossy());
            let state=match session_ownership(&entries){SessionOwnership::Writable=>"writable",SessionOwnership::Refused(owner)=>if owner.state==RefusedState::Moved{"moved"}else{"moving"}};
            Ok(json!({"ownership":state}))
        },
        "import"=>{
            let cwd=Path::new(text(command,"cwd")?);let dir=PathBuf::from(maho_core::session_manager::get_default_session_dir_with(&cwd.to_string_lossy(),agent_dir));
            let outcome=maho_core::session_move::import::import_moved_session(maho_core::session_move::import::ImportMovedSessionOptions{bundle_dir:Path::new(text(command,"bundleDir")?),target_cwd:cwd,ledger:&ledger,session_dir:Some(&dir),from:command["from"].as_str(),read_identity:None}).await?;
            let entries=maho_core::session_manager::load_entries_from_file(&outcome.to_string_lossy());
            Ok(json!({"session_id":maho_core::session_move::ownership::header_session_id(&entries),"session_path":outcome}))
        },
        "revoke"=>{
            let path=Path::new(text(command,"sessionPath")?);let guard=||closed(hosted_paths,&path.to_string_lossy());
            maho_core::session_move::revoke::revoke_imported_move(maho_core::session_move::revoke::RevokeMoveOptions{session_file:path,move_id:id,ledger:&ledger,guard:Some(&guard)}).await?;Ok(json!({"ownership":"moved"}))
        },
        _=>Err("invalid_move_request: invalid move operation".into()),
    }}.await;
    match result {
        Ok(data) => {
            json!({"id":command["id"],"type":"response","command":"move_session","success":true,"data":data})
        }
        Err(error) => {
            let code = error
                .split(':')
                .next()
                .filter(|code| {
                    matches!(
                        *code,
                        "invalid_move_request"
                            | "session_open_here"
                            | "move_not_released"
                            | "move_mismatch"
                            | "move_bundle_invalid"
                            | "move_repository_mismatch"
                            | "move_target_occupied"
                            | "move_already_moved"
                            | "move_fenced"
                    )
                })
                .unwrap_or("move_failed");
            refusal(command, code, &error)
        }
    }
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
fn text<'a>(command: &'a Value, key: &str) -> Result<&'a str, String> {
    command[key]
        .as_str()
        .filter(|text| !text.trim().is_empty())
        .ok_or_else(|| format!("invalid_move_request: {key} must be a non-empty string"))
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
        assert!(validate_move_request(&json!({"op":"finish","moveId":"m","sessionPath":"/session","answer":{"state":"accepted","revoked":true}})).is_ok());
    }
}
