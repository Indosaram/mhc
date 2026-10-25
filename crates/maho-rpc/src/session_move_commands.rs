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
        MoveStatus::Accepted => json!({"state":"accepted"}),
        MoveStatus::Declined { reason } => json!({"state":"declined","reason":reason.as_str()}),
        MoveStatus::Failed { reason } => json!({"state":"failed","reason":reason.as_str()}),
    }
}
fn refusal(command: &Value, code: &str, detail: &str) -> Value {
    json!({"id":command["id"],"type":"response","command":"move_session","success":false,"error":code,"errorCode":code,"errorData":{"detail":detail}})
}
pub async fn move_session(agent_dir: &str, hosted_paths: &[String], command: &Value) -> Value {
    let op = command["op"].as_str().unwrap_or_default();
    if op != "import" && !bundle::is_move_id(&command["moveId"]) {
        return refusal(
            command,
            "invalid_move_request",
            "moveId must be a plain id: a letter or digit, then letters, digits, '.', '_' or '-', at most 128 characters.",
        );
    }
    let _lock = if matches!(op, "export" | "finish" | "revoke") {
        match text(command, "sessionPath") {
            Ok(path) => {
                match maho_core::lockfile_policy::acquire_lock_async(&format!("{path}.move"), 15000)
                    .await
                {
                    Ok(guard) => Some(guard),
                    Err(error) => return refusal(command, "move_failed", &error.to_string()),
                }
            }
            Err(error) => return refusal(command, "invalid_move_request", &error),
        }
    } else {
        None
    };
    let root = Path::new(agent_dir).join("session-moves");
    let ledger = MoveLedger::new(root.join("ledger"));
    let id = command["moveId"].as_str().unwrap_or_default();
    let result:Result<Value,String>=async{match op{
        "prepare"=>ledger.prepare(id).await.map(wire).map_err(|error|error.to_string()),
        "cancel"=>ledger.decline(id,DeclineReason::Cancelled).await.map(wire).map_err(|error|error.to_string()),
        "status"=>{let path=command["sessionPath"].as_str();let moved=path.is_some_and(|path|maho_core::session_manager::load_entries_from_file(path).iter().any(|entry|entry["customType"]=="session_moved_in"&&entry["data"]["move_id"]==id));if path.is_some_and(|path|Path::new(path).exists())&&!moved{return Err("move_mismatch: sessionPath is not this move's import".into());}ledger.answer_status(id,moved).await.map(status_wire).map_err(|error|error.to_string())},
        "export"=>export(&root,hosted_paths,command),
        "finish"=>finish(&root,hosted_paths,command),
        "import"=>import(agent_dir,&ledger,command).await,
        "revoke"=>revoke(hosted_paths,command),
        _=>Err("invalid_move_request: op must be one of prepare, cancel, status, export, import, finish or revoke".into()),
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
fn copy_tree(from: &Path, to: &Path, base: &Path, files: &mut Vec<Value>) -> Result<(), String> {
    if !from.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(from).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path();
        if entry.file_type().map_err(|e| e.to_string())?.is_dir() {
            copy_tree(&path, to, base, files)?;
        } else {
            let relative = path.strip_prefix(base).map_err(|e| e.to_string())?;
            let target = to.join(relative);
            std::fs::create_dir_all(target.parent().ok_or("invalid parent")?)
                .map_err(|e| e.to_string())?;
            std::fs::copy(&path, &target).map_err(|e| e.to_string())?;
            files.push(json!({"path":format!("files/{}",relative.to_string_lossy().replace('\\',"/")),"sha256":bundle::sha256_file(&target).map_err(|e|e.to_string())?}));
        }
    }
    Ok(())
}
fn export(root: &Path, hosted: &[String], command: &Value) -> Result<Value, String> {
    let path = text(command, "sessionPath")?;
    let target = text(command, "target")?;
    closed(hosted, path)?;
    let id = text(command, "moveId")?;
    let entries = maho_core::session_manager::load_entries_from_file(path);
    let session_id = maho_core::session_move::ownership::header_session_id(&entries)
        .ok_or("move_bundle_invalid: missing session id")?;
    if !bundle::is_plain_id(&json!(session_id)) {
        return Err("move_bundle_invalid: session id is not plain".into());
    }
    if command.get("droppedUserMessages").is_some_and(|value| {
        !value
            .as_array()
            .is_some_and(|items| items.iter().all(Value::is_string))
    }) {
        return Err("invalid_move_request: droppedUserMessages must be strings".into());
    }
    if !matches!(session_ownership(&entries),SessionOwnership::Refused(ref hold) if hold.state==RefusedState::Moving&&hold.move_id==id)
    {
        return Err("move_not_released: release_session reason move with this moveId first".into());
    }
    let dir = root.join("outgoing").join(id);
    let staging = root
        .join("outgoing")
        .join(format!("{id}.partial-{}", std::process::id()));
    if staging.exists() {
        std::fs::remove_dir_all(&staging).map_err(|e| e.to_string())?;
    }
    std::fs::create_dir_all(staging.join("files")).map_err(|e| e.to_string())?;
    std::fs::copy(path, staging.join(bundle::SESSION_BUNDLE_PATH)).map_err(|e| e.to_string())?;
    let parent = Path::new(path).parent().ok_or("invalid session parent")?;
    let mut sidecars = Vec::new();
    copy_tree(
        &parent.join("resident-blobs").join(session_id),
        &staging.join("files"),
        parent,
        &mut sidecars,
    )?;
    let extensions = parent.join("extensions");
    if extensions.exists() {
        for entry in std::fs::read_dir(&extensions).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            let file = entry.path().join(format!("{session_id}.json"));
            if file.is_file() {
                let relative = file.strip_prefix(parent).map_err(|e| e.to_string())?;
                let dest = staging.join("files").join(relative);
                std::fs::create_dir_all(dest.parent().ok_or("invalid parent")?)
                    .map_err(|e| e.to_string())?;
                std::fs::copy(&file, &dest).map_err(|e| e.to_string())?;
                sidecars.push(json!({"path":format!("files/{}",relative.to_string_lossy()),"sha256":bundle::sha256_file(&dest).map_err(|e|e.to_string())?}));
            }
        }
    }
    let manifest = json!({"format":1,"moveId":id,"sessionId":session_id,"sessionFileName":Path::new(path).file_name().and_then(|name|name.to_str()).ok_or("invalid file name")?,"sourceCwd":entries.first().and_then(|entry|entry["cwd"].as_str()).unwrap_or_default(),"repository":entries.iter().rev().find(|entry|entry["customType"]=="repository-identity").map(|entry|entry["data"].clone()).unwrap_or(Value::Null),"session":{"path":bundle::SESSION_BUNDLE_PATH,"sha256":bundle::sha256_file(&staging.join(bundle::SESSION_BUNDLE_PATH)).map_err(|e|e.to_string())?},"sidecars":sidecars,"droppedUserMessages":command.get("droppedUserMessages").cloned().unwrap_or_else(||json!([]))});
    std::fs::write(staging.join(bundle::MANIFEST_FILE), manifest.to_string())
        .map_err(|e| e.to_string())?;
    if dir.exists() {
        std::fs::remove_dir_all(&dir).map_err(|e| e.to_string())?;
    }
    std::fs::rename(staging, &dir).map_err(|e| e.to_string())?;
    Ok(json!({"move_id":id,"session_id":session_id,"bundle_dir":dir,"files":1+sidecars.len()}))
}
fn finish(root: &Path, hosted: &[String], command: &Value) -> Result<Value, String> {
    let path = text(command, "sessionPath")?;
    closed(hosted, path)?;
    let id = text(command, "moveId")?;
    let answer = command["answer"]["state"]
        .as_str()
        .ok_or("invalid_move_request: finish requires answer")?;
    if !matches!(answer, "receiving" | "accepted" | "declined" | "failed") {
        return Err("invalid_move_request: invalid answer state".into());
    }
    let entries = maho_core::session_manager::load_entries_from_file(path);
    let ownership = session_ownership(&entries);
    if let SessionOwnership::Refused(ref hold) = ownership {
        if hold.move_id != id {
            return Err("move_mismatch: session belongs to another move".into());
        }
        if answer == "receiving" {
            return Ok(json!({"ownership":"moving"}));
        }
        if answer == "accepted"
            && command["answer"]["revoked"] == true
            && hold.state == RefusedState::Moved
        {
            return Err(
                "move_already_moved: receiver revoked a session already marked moved".into(),
            );
        }
        if hold.state == RefusedState::Moved {
            return Ok(json!({"ownership":"moved"}));
        }
        let take_back = answer != "accepted" || command["answer"]["revoked"] == true;
        let mut manager = maho_core::session_manager::SessionManager::open(path, None, None, None)?;
        let session_id = manager.session_id().to_owned();
        manager.append_custom(
            if !take_back {
                "session_moved"
            } else {
                "session_reclaimed"
            },
            Some(
                json!({"move_id":id,"session_id":session_id,"target":hold.target,"reason":answer}),
            ),
        );
        if take_back {
            let kept = root.join("outgoing").join(id);
            if kept.exists() {
                let manifest = bundle::read_manifest(&kept).map_err(|e| e.to_string())?;
                for (index, text) in manifest.dropped_user_messages.iter().enumerate() {
                    let input_id = format!("move:{id}:{index}");
                    if !manager
                        .entries()
                        .iter()
                        .any(|entry| entry["data"]["inputId"] == input_id)
                    {
                        manager.append_custom(
                            "held-input",
                            Some(
                                json!({"inputId":input_id,"revision":0,"text":text,"prepare":true}),
                            ),
                        );
                    }
                }
            }
        }
    }
    let dir = root.join("outgoing").join(id);
    if dir.exists() {
        std::fs::remove_dir_all(dir).map_err(|e| e.to_string())?;
    }
    Ok(json!({"ownership":if answer=="accepted"{"moved"}else{"writable"}}))
}
async fn import(agent_dir: &str, ledger: &MoveLedger, command: &Value) -> Result<Value, String> {
    let bundle_dir = Path::new(text(command, "bundleDir")?);
    let cwd = text(command, "cwd")?;
    if !bundle_dir.is_absolute() || !Path::new(cwd).is_absolute() {
        return Err(
            "invalid_move_request: import needs bundleDir and cwd as absolute paths".into(),
        );
    }
    let manifest = bundle::read_manifest(bundle_dir).map_err(|e| e.to_string())?;
    bundle::verify_bundle(bundle_dir, &manifest).map_err(|e| e.to_string())?;
    let target_identity = maho_core::repository_identity::read_repository_identity(cwd).await;
    if maho_core::repository_identity::compare_repository_identities(
        manifest.repository.as_ref(),
        target_identity.as_ref(),
    ) != "same"
    {
        ledger
            .decline(&manifest.move_id, DeclineReason::Repository)
            .await
            .map_err(|e| e.to_string())?;
        return Err(
            "move_repository_mismatch: repository identity must match the destination checkout"
                .into(),
        );
    }
    let dir = PathBuf::from(maho_core::session_manager::get_default_session_dir_with(
        cwd, agent_dir,
    ));
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let target = dir.join(&manifest.session_file_name);
    if target.exists()
        && matches!(
            session_ownership(&maho_core::session_manager::load_entries_from_file(
                &target.to_string_lossy()
            )),
            SessionOwnership::Writable
        )
    {
        return Err("move_target_occupied: writable session already exists".into());
    }
    ledger
        .begin_receive(&manifest.move_id)
        .await
        .map_err(|e| e.to_string())?;
    let staged = dir.join(format!(".move-{}", manifest.move_id));
    std::fs::copy(bundle_dir.join(&manifest.session.path), &staged).map_err(|e| e.to_string())?;
    let mut staged_manager = maho_core::session_manager::SessionManager::open(
        &staged.to_string_lossy(),
        None,
        Some(cwd),
        None,
    )?;
    staged_manager.append_custom("session_moved_in",Some(json!({"move_id":manifest.move_id,"session_id":manifest.session_id,"from":command["from"]})));
    for (index, text) in manifest.dropped_user_messages.iter().enumerate() {
        staged_manager.append_custom("held-input",Some(json!({"inputId":format!("move:{}:{index}",manifest.move_id),"revision":0,"text":text,"prepare":true})));
    }
    drop(staged_manager);
    for sidecar in &manifest.sidecars {
        let relative = sidecar
            .path
            .strip_prefix("files/")
            .ok_or("move_bundle_invalid: sidecar outside files")?;
        let dest = dir.join(relative);
        std::fs::create_dir_all(dest.parent().ok_or("invalid parent")?)
            .map_err(|e| e.to_string())?;
        std::fs::copy(bundle_dir.join(&sidecar.path), dest).map_err(|e| e.to_string())?;
    }
    ledger
        .commit(&manifest.move_id, || {
            std::fs::rename(&staged, &target).map_err(|e| e.to_string())?;
            Ok(())
        })
        .await
        .map_err(|e| e.to_string())?;
    Ok(json!({"session_id":manifest.session_id,"session_path":target}))
}
fn revoke(hosted: &[String], command: &Value) -> Result<Value, String> {
    let path = text(command, "sessionPath")?;
    closed(hosted, path)?;
    let id = text(command, "moveId")?;
    let entries = maho_core::session_manager::load_entries_from_file(path);
    if !entries
        .iter()
        .any(|entry| entry["customType"] == "session_moved_in" && entry["data"]["move_id"] == id)
    {
        return Err("move_mismatch: not this move's imported copy".into());
    }
    let latest = entries.iter().rev().find(|entry| {
        matches!(
            entry["customType"].as_str(),
            Some("session_moved_in" | "session_moved" | "session_reclaimed" | "session_released")
        )
    });
    if latest.is_some_and(|entry| {
        entry["customType"] == "session_moved"
            && entry["data"]["move_id"] == id
            && entry["data"]["reason"] == "revoked"
    }) {
        return Ok(json!({"ownership":"moved"}));
    }
    if !latest.is_some_and(|entry| {
        entry["customType"] == "session_moved_in" && entry["data"]["move_id"] == id
    }) || !matches!(session_ownership(&entries), SessionOwnership::Writable)
    {
        return Err("move_mismatch: not this move's current imported copy".into());
    }
    let mut manager = maho_core::session_manager::SessionManager::open(path, None, None, None)?;
    let session_id = manager.session_id().to_owned();
    manager.append_custom(
        "session_moved",
        Some(json!({"move_id":id,"session_id":session_id,"reason":"revoked"})),
    );
    Ok(json!({"ownership":"moved"}))
}
