use crate::{errors::GoalError, types::{Goal, GoalFile, GoalStoreRef}, validation::{is_goal_status, is_non_negative_safe_integer}};
use std::{fs, io::Write, path::{Path, PathBuf}};
use serde_json::Value;
pub fn encoded_thread_id(reference: &GoalStoreRef) -> String {
    let mut encoded = String::new();
    for byte in reference.thread_id.bytes() {
        match byte { b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'!' | b'~' | b'*' | b'\'' | b'(' | b')' => encoded.push(char::from(byte)), _ => encoded.push_str(&format!("%{byte:02X}")) }
    }
    encoded
}
pub fn goal_file_path(reference: &GoalStoreRef) -> PathBuf { reference.base_dir.join(format!("{}.json", encoded_thread_id(reference))) }
pub fn read_goal_file(reference: &GoalStoreRef) -> Result<Option<Goal>, GoalError> {
    match fs::read_to_string(goal_file_path(reference)) { Ok(raw) => Ok(parse_goal_file(&raw, false)?.goal), Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None), Err(error) => Err(GoalError::Io(error.to_string())) }
}
fn json_parse(raw: &str) -> Result<Value, GoalError> {
    match serde_json::from_str(raw) {
        Ok(value) => Ok(value),
        Err(original) => match recover_stale_closing_braces(raw).and_then(|recovered| serde_json::from_str(recovered).ok()) { Some(value) => Ok(value), None => Err(GoalError::Json(original.to_string())) },
    }
}
fn recover_stale_closing_braces(raw: &str) -> Option<&str> {
    let start = raw.bytes().position(|b| !matches!(b, b'\t' | b'\n' | b'\r' | b' '))?;
    if raw.as_bytes()[start] != b'{' { return None; }
    let mut depth = 0_i64;
    let mut in_string = false;
    let mut escaped = false;
    for (index, byte) in raw.bytes().enumerate().skip(start) {
        if in_string { if escaped { escaped = false; } else if byte == b'\\' { escaped = true; } else if byte == b'"' { in_string = false; } continue; }
        match byte {
            b'"' => in_string = true,
            b'{' | b'[' => depth += 1,
            b'}' | b']' => {
                depth -= 1;
                if depth < 0 { return None; }
                if depth == 0 {
                    let suffix = &raw.as_bytes()[index + 1..];
                    return (suffix.contains(&b'}') && suffix.iter().all(|b| matches!(b, b'}' | b' ' | b'\t' | b'\n' | b'\r'))).then_some(&raw[..index + 1]);
                }
            }
            _ => (),
        }
    }
    None
}
fn safe(value: &Value) -> bool { value.as_f64().is_some_and(is_non_negative_safe_integer) }
pub fn parse_goal_file(raw: &str, legacy: bool) -> Result<GoalFile, GoalError> {
    let parsed = json_parse(raw)?;
    let object = parsed.as_object().ok_or_else(|| GoalError::InvalidStore("goal store must be a JSON object".into()))?;
    if object.get("version").and_then(Value::as_f64) != Some(1.0) { return Err(GoalError::UnsupportedStoreVersion("unsupported goal store version".into())); }
    let mut value = object.get("goal").cloned().ok_or_else(|| GoalError::InvalidStore("goal store contains an invalid goal".into()))?;
    if value.is_null() { return Ok(GoalFile { version: 1, goal: None }); }
    let fields = value.as_object_mut().ok_or_else(|| GoalError::InvalidStore("goal store contains an invalid goal".into()))?;
    if legacy { fields.remove("tokenBudget"); if matches!(fields.get("status").and_then(Value::as_str), Some("budgetLimited" | "budget_limited")) { fields.insert("status".into(), Value::String("active".into())); } }
    let strings_valid = ["id", "threadId", "objective"].iter().all(|key| fields.get(*key).is_some_and(Value::is_string));
    let status_valid = fields.get("status").is_some_and(is_goal_status);
    let required_numbers = ["tokensUsed", "timeUsedSeconds", "createdAt", "updatedAt"].iter().all(|key| fields.get(*key).is_some_and(safe));
    let optional_numbers = ["tokenBudget", "lastStartedAt", "completedAt"].iter().all(|key| fields.get(*key).is_none_or(safe));
    let blocked = fields.get("status").and_then(Value::as_str) == Some("blocked");
    let blocked_valid = if blocked { fields.get("blockedReason").and_then(Value::as_str).is_some_and(|s| !s.trim_matches(crate::validation::js_whitespace).is_empty()) && fields.get("blockedAt").is_some_and(safe) } else { !fields.contains_key("blockedReason") && !fields.contains_key("blockedAt") };
    if !(strings_valid && status_valid && required_numbers && optional_numbers && blocked_valid) { return Err(GoalError::InvalidStore("goal store contains an invalid goal".into())); }
    for key in ["consecutiveContinuations", "unattendedContinuations"] { if fields.get(key).is_none_or(|v| !safe(v)) { fields.remove(key); } }
    if fields.get("lastContinuationSignature").is_none_or(|v| !v.is_string()) { fields.remove("lastContinuationSignature"); }
    for key in ["tokensUsed","createdAt","updatedAt","tokenBudget","lastStartedAt","completedAt","blockedAt","consecutiveContinuations","unattendedContinuations"] {
        if let Some(number)=fields.get(key).and_then(Value::as_f64) { fields.insert(key.into(),Value::from(number as u64)); }
    }
    let goal = serde_json::from_value(value).map_err(|error| GoalError::InvalidStore(error.to_string()))?;
    Ok(GoalFile { version: 1, goal: Some(goal) })
}
fn contents(goal: Option<&Goal>) -> Result<String, GoalError> {
    let value=serde_json::to_value(GoalFile { version:1,goal:goal.cloned() }).map_err(|error|GoalError::Json(error.to_string()))?;
    Ok(format!("{}\n",maho_ai::utils::js::json_stringify_pretty(&value)))
}
fn write_private(path: &Path, contents: &str, exclusive: bool) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let mut options = fs::OpenOptions::new(); options.write(true).mode(0o600);
    if exclusive { options.create_new(true); } else { options.create(true).truncate(true); }
    options.open(path)?.write_all(contents.as_bytes())
}
pub fn write_goal_file(reference: &GoalStoreRef, goal: Option<&Goal>) -> Result<(), GoalError> {
    let path = goal_file_path(reference);
    fs::create_dir_all(&reference.base_dir).map_err(|error| GoalError::Io(error.to_string()))?;
    let text = contents(goal)?;
    let temp = reference.base_dir.join(format!(".goal-{}.tmp", uuid::Uuid::new_v4()));
    if let Err(error) = write_private(&temp, &text, false).and_then(|()| fs::rename(&temp, &path)) {
        if let Err(cleanup) = fs::remove_file(&temp)
            && cleanup.kind() != std::io::ErrorKind::NotFound { return Err(GoalError::Io(format!("goal store write failed and its temporary file could not be removed: {error}; {cleanup}"))); }
        return Err(GoalError::Io(error.to_string()));
    }
    Ok(())
}
fn retire_legacy(path: &Path) { let _=fs::rename(path, format!("{}.migrated", path.display())); }
fn read_legacy_candidate(path: &Path) -> Result<Option<Goal>, GoalError> {
    let raw = match fs::read_to_string(path) { Ok(raw) => raw, Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None), Err(error) => return Err(GoalError::Io(error.to_string())) };
    match parse_goal_file(&raw, true) {
        Ok(file) => { if file.goal.is_none() { retire_legacy(path); } Ok(file.goal) },
        Err(GoalError::InvalidStore(_) | GoalError::UnsupportedStoreVersion(_) | GoalError::Json(_)) => Ok(None),
        Err(error) => Err(error),
    }
}
pub fn migrate_legacy_goal_file(reference: &GoalStoreRef, standalone_agent_dir: &Path) -> Result<Option<Goal>, GoalError> {
    migrate_legacy_goal_file_with_publish(reference,standalone_agent_dir,|path,text|write_private(path,text,true))
}
pub fn migrate_legacy_goal_file_default(reference:&GoalStoreRef)->Result<Option<Goal>,GoalError> {
    let standalone=std::env::var_os("PI_CODING_AGENT_DIR").map(PathBuf::from).or_else(||dirs::home_dir().map(|home|home.join(".pi/agent"))).ok_or_else(||GoalError::Io("operating-system home directory unavailable".into()))?;
    migrate_legacy_goal_file(reference,&standalone)
}
fn migrate_legacy_goal_file_with_publish(reference:&GoalStoreRef,standalone_agent_dir:&Path,publish:impl FnOnce(&Path,&str)->std::io::Result<()>)->Result<Option<Goal>,GoalError> {
    match fs::read_to_string(goal_file_path(reference)) { Ok(_) => return Ok(None), Err(error) if error.kind() == std::io::ErrorKind::NotFound => (), Err(error) => return Err(GoalError::Io(error.to_string())) }
    let base = reference.base_dir.to_string_lossy();
    let mut segments: Vec<_> = base.split(['/', '\\']).collect();
    let Some(index) = segments.iter().rposition(|s| *s == "goal") else { return Ok(None); };
    let cwd_key = if segments.get(index + 1) == Some(&"no-session") { segments.get(index + 2).copied() } else { None };
    segments[index] = "pi-goal";
    let legacy_base = PathBuf::from(segments.join("/"));
    let mut paths = Vec::new();
    if let Some(key) = cwd_key {
        let dirs = std::collections::BTreeSet::from([legacy_base, standalone_agent_dir.join("extensions/pi-goal/no-session").join(key)]);
        for dir in dirs {
            let entries = match fs::read_dir(dir) { Ok(entries) => entries, Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue, Err(error) => return Err(GoalError::Io(error.to_string())) };
            for entry in entries { let entry = entry.map_err(|error| GoalError::Io(error.to_string()))?; if entry.file_type().map_err(|error| GoalError::Io(error.to_string()))?.is_file() && entry.file_name().to_string_lossy().ends_with(".json") { paths.push(entry.path()); } }
        }
    } else { paths.push(goal_file_path(&GoalStoreRef { base_dir: legacy_base, thread_id: reference.thread_id.clone() })); }
    paths.sort_by(|left,right|left.to_string_lossy().encode_utf16().cmp(right.to_string_lossy().encode_utf16()));
    let mut candidates = Vec::new();
    for path in paths { if let Some(goal) = read_legacy_candidate(&path)? { candidates.push((path, goal)); } }
    if candidates.len() > 1 { return Err(GoalError::InvalidMutation(format!("multiple legacy goals found for no-session store: {}", candidates.iter().map(|(p, _)| p.to_string_lossy()).collect::<Vec<_>>().join(", ")))); }
    let Some((path, goal)) = candidates.pop() else { return Ok(None); };
    fs::create_dir_all(&reference.base_dir).map_err(|error| GoalError::Io(error.to_string()))?;
    let published = match publish(&goal_file_path(reference), &contents(Some(&goal))?) { Ok(()) => true, Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => false, Err(error) => return Err(GoalError::Io(error.to_string())) };
    retire_legacy(&path);
    Ok(published.then_some(goal))
}
#[cfg(test)] mod tests {
    use super::*;
    fn raw() -> String { serde_json::json!({"version":1,"goal":{"id":"g","threadId":"t","objective":"work","status":"active","tokensUsed":0,"timeUsedSeconds":0,"createdAt":1,"updatedAt":1}}).to_string() }
    #[test] fn default_migration_honors_distinct_standalone_root() {
        if let Some(root)=std::env::var_os("MAHO_GOAL_MIGRATION_TEST_ROOT") {
            let reference=GoalStoreRef { base_dir:PathBuf::from(root).join("extensions/goal/no-session/key"),thread_id:"new".into() }; assert_eq!(migrate_legacy_goal_file_default(&reference).unwrap().unwrap().id,"g"); return;
        }
        let dir=tempfile::tempdir().unwrap(); let standalone=tempfile::tempdir().unwrap(); let legacy=standalone.path().join("extensions/pi-goal/no-session/key"); fs::create_dir_all(&legacy).unwrap(); fs::write(legacy.join("old.json"),raw()).unwrap();
        let result=std::process::Command::new(std::env::current_exe().unwrap()).args(["--exact","persistence::tests::default_migration_honors_distinct_standalone_root","--nocapture"]).env("MAHO_GOAL_MIGRATION_TEST_ROOT",dir.path()).env("PI_CODING_AGENT_DIR",standalone.path()).output().unwrap();
        assert!(result.status.success(),"{}",String::from_utf8_lossy(&result.stderr)); assert!(legacy.join("old.json.migrated").exists()); assert!(dir.path().join("extensions/goal/no-session/key/new.json").exists());
    }
    #[test] fn legacy_retirement_failure_does_not_fail_live_publication() {
        let dir=tempfile::tempdir().unwrap(); let reference=GoalStoreRef { base_dir:dir.path().join("extensions/goal"),thread_id:"t".into() }; let legacy=dir.path().join("extensions/pi-goal"); fs::create_dir_all(&legacy).unwrap(); fs::write(legacy.join("t.json"),raw()).unwrap(); fs::create_dir(legacy.join("t.json.migrated")).unwrap();
        let imported=migrate_legacy_goal_file(&reference,dir.path()).unwrap().unwrap(); assert_eq!(read_goal_file(&reference).unwrap(),Some(imported)); assert!(legacy.join("t.json").exists()); assert!(migrate_legacy_goal_file(&reference,dir.path()).unwrap().is_none());
    }
    #[test] fn migration_conflict_paths_follow_javascript_utf16_order() {
        let dir=tempfile::tempdir().unwrap(); let reference=GoalStoreRef { base_dir:dir.path().join("extensions/goal/no-session/key"),thread_id:"t".into() }; let legacy=dir.path().join("extensions/pi-goal/no-session/key"); fs::create_dir_all(&legacy).unwrap();
        for name in ["\u{e000}.json","\u{10000}.json"] { fs::write(legacy.join(name),raw()).unwrap(); }
        let error=migrate_legacy_goal_file(&reference,dir.path()).unwrap_err().to_string();
        assert!(error.find("\u{10000}.json").unwrap()<error.find("\u{e000}.json").unwrap()); assert!(!goal_file_path(&reference).exists());
    }
    #[test] fn migration_propagates_current_read_and_publication_io_failures() {
        let dir=tempfile::tempdir().unwrap(); let reference=GoalStoreRef { base_dir:dir.path().join("extensions/goal"),thread_id:"t".into() }; let legacy=dir.path().join("extensions/pi-goal"); fs::create_dir_all(&legacy).unwrap(); fs::write(legacy.join("t.json"),raw()).unwrap();
        let error=migrate_legacy_goal_file_with_publish(&reference,dir.path(),|_,_|Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied,"publication denied"))).unwrap_err(); assert!(matches!(error,GoalError::Io(_))); assert!(legacy.join("t.json").exists()); assert!(!legacy.join("t.json.migrated").exists());
        fs::create_dir_all(goal_file_path(&reference)).unwrap(); assert!(matches!(migrate_legacy_goal_file(&reference,dir.path()),Err(GoalError::Io(_))));
    }
    #[test] fn upstream_atomic_replacement_preserves_private_mode() {
        use std::os::unix::fs::PermissionsExt;
        let dir=tempfile::tempdir().unwrap(); let reference=GoalStoreRef { base_dir:dir.path().join("goal"),thread_id:"thread-private-mode".into() };
        let mut goal=parse_goal_file(&raw(),false).unwrap().goal.unwrap(); write_goal_file(&reference,Some(&goal)).unwrap();
        fs::set_permissions(goal_file_path(&reference),fs::Permissions::from_mode(0o600)).unwrap(); goal.objective="Still private".into(); write_goal_file(&reference,Some(&goal)).unwrap();
        assert_eq!(fs::metadata(goal_file_path(&reference)).unwrap().permissions().mode()&0o777,0o600); assert_eq!(read_goal_file(&reference).unwrap(),Some(goal));
    }
    #[test] fn upstream_current_writer_wins_at_exclusive_migration_publication() {
        let dir=tempfile::tempdir().unwrap(); let reference=GoalStoreRef { base_dir:dir.path().join("extensions/goal"),thread_id:"t".into() }; let legacy=dir.path().join("extensions/pi-goal"); fs::create_dir_all(&legacy).unwrap(); fs::write(legacy.join("t.json"),raw()).unwrap();
        let mut current=parse_goal_file(&raw(),false).unwrap().goal.unwrap(); current.objective="current writer".into();
        let result=migrate_legacy_goal_file_with_publish(&reference,dir.path(),|path,text| { write_goal_file(&reference,Some(&current)).unwrap(); write_private(path,text,true) }).unwrap();
        assert!(result.is_none()); assert_eq!(read_goal_file(&reference).unwrap(),Some(current)); assert!(legacy.join("t.json.migrated").exists()); assert_eq!(fs::read_dir(&reference.base_dir).unwrap().count(),1);
    }
    #[test] fn upstream_adversarial_stale_brace_suffix_preserves_original_error() {
        let input=format!("{}{}X",raw(),"} ".repeat(24));
        let original=serde_json::from_str::<Value>(&input).unwrap_err().to_string();
        assert!(matches!(parse_goal_file(&input,false),Err(GoalError::Json(error)) if error==original));
    }
    #[test] fn upstream_stale_brace_recovery_still_rejects_unsupported_versions() {
        assert!(matches!(parse_goal_file("{\"version\":2,\"goal\":null}\n}\n",false),Err(GoalError::UnsupportedStoreVersion(_))));
    }
    #[test] fn upstream_supported_legacy_status_drops_budget_and_corrupt_tracking() {
        let mut input:Value=serde_json::from_str(&raw()).unwrap(); input["goal"]["tokenBudget"]=100.into(); input["goal"]["consecutiveContinuations"]="many".into(); input["goal"]["lastContinuationSignature"]=42.into();
        let goal=parse_goal_file(&input.to_string(),true).unwrap().goal.unwrap(); assert_eq!(goal.status,crate::types::GoalStatus::Active); assert!(goal.token_budget.is_none()); assert!(goal.consecutive_continuations.is_none()); assert!(goal.last_continuation_signature.is_none());
    }
    #[test] fn upstream_nested_legacy_decoy_is_not_migrated() {
        let dir=tempfile::tempdir().unwrap(); let reference=GoalStoreRef { base_dir:dir.path().join("extensions/goal/no-session").join("b".repeat(24)),thread_id:"t".into() };
        let decoy=dir.path().join("extensions/goal/no-session/pi-goal"); fs::create_dir_all(&decoy).unwrap(); fs::write(decoy.join("t.json"),raw()).unwrap();
        assert!(migrate_legacy_goal_file(&reference,dir.path()).unwrap().is_none()); assert!(read_goal_file(&reference).unwrap().is_none());
    }
    #[tokio::test] async fn upstream_absent_and_corrupt_tracking_default_to_zero_on_delivery() {
        for invalid in [None,Some(serde_json::json!("eight")),Some(serde_json::json!(-3))] {
            let dir=tempfile::tempdir().unwrap(); let reference=GoalStoreRef { base_dir:dir.path().into(),thread_id:"t".into() }; let mut value:Value=serde_json::from_str(&raw()).unwrap();
            if let Some(invalid)=invalid { value["goal"]["consecutiveContinuations"]=invalid; value["goal"]["lastContinuationSignature"]=42.into(); }
            fs::write(goal_file_path(&reference),value.to_string()).unwrap(); let goal=read_goal_file(&reference).unwrap().unwrap(); assert!(goal.consecutive_continuations.is_none()); assert!(goal.last_continuation_signature.is_none());
            let delivered=crate::store::record_continuation_delivered(&reference,"signature",Some(&goal.id),true).await.unwrap().unwrap(); assert_eq!(delivered.consecutive_continuations,Some(1));
        }
    }
    #[test] fn upstream_standalone_root_migration_publishes_private_file() {
        use std::os::unix::fs::PermissionsExt;
        let dir=tempfile::tempdir().unwrap(); let standalone=tempfile::tempdir().unwrap(); let key="d".repeat(24);
        let reference=GoalStoreRef { base_dir:dir.path().join("extensions/goal/no-session").join(&key),thread_id:"new".into() };
        let legacy=standalone.path().join("extensions/pi-goal/no-session").join(key); fs::create_dir_all(&legacy).unwrap(); fs::write(legacy.join("old.json"),raw()).unwrap();
        let migrated=migrate_legacy_goal_file(&reference,standalone.path()).unwrap().unwrap(); assert_eq!(read_goal_file(&reference).unwrap(),Some(migrated));
        assert_eq!(fs::metadata(goal_file_path(&reference)).unwrap().permissions().mode()&0o777,0o600); assert!(!legacy.join("old.json").exists()); assert!(legacy.join("old.json.migrated").exists());
    }
    #[test] fn upstream_overlapping_writes_leave_exactly_one_submitted_envelope() {
        let dir=tempfile::tempdir().unwrap(); let reference=GoalStoreRef { base_dir:dir.path().into(),thread_id:"t".into() };
        let first=parse_goal_file(&raw(),false).unwrap().goal.unwrap(); let mut second=first.clone(); second.objective="replacement".into();
        let barrier=std::sync::Barrier::new(3);
        std::thread::scope(|scope| {
            scope.spawn(|| { barrier.wait(); write_goal_file(&reference,Some(&first)).unwrap(); });
            scope.spawn(|| { barrier.wait(); write_goal_file(&reference,Some(&second)).unwrap(); });
            barrier.wait();
        });
        let actual=fs::read_to_string(goal_file_path(&reference)).unwrap(); assert!(actual==contents(Some(&first)).unwrap()||actual==contents(Some(&second)).unwrap()); assert_eq!(fs::read_dir(dir.path()).unwrap().count(),1);
    }
    #[test] fn upstream_atomic_write_accepts_maximum_valid_basename() {
        let dir=tempfile::tempdir().unwrap(); let reference=GoalStoreRef { base_dir:dir.path().into(),thread_id:"x".repeat(250) };
        assert_eq!(goal_file_path(&reference).file_name().unwrap().as_encoded_bytes().len(),255);
        let goal=parse_goal_file(&raw(),false).unwrap().goal.unwrap(); write_goal_file(&reference,Some(&goal)).unwrap(); assert_eq!(read_goal_file(&reference).unwrap(),Some(goal));
    }
    #[test] fn upstream_rename_failure_removes_temporary_sibling() {
        let dir=tempfile::tempdir().unwrap(); let reference=GoalStoreRef { base_dir:dir.path().into(),thread_id:"t".into() };
        fs::create_dir(goal_file_path(&reference)).unwrap(); let goal=parse_goal_file(&raw(),false).unwrap().goal.unwrap();
        assert!(write_goal_file(&reference,Some(&goal)).is_err()); assert_eq!(fs::read_dir(dir.path()).unwrap().count(),1);
    }
    #[test] fn upstream_no_session_migration_preserves_old_thread_identity() {
        let dir=tempfile::tempdir().unwrap(); let key="a".repeat(24);
        let reference=GoalStoreRef { base_dir:dir.path().join("extensions/goal/no-session").join(&key),thread_id:"new-thread".into() };
        let legacy=dir.path().join("extensions/pi-goal/no-session").join(key); fs::create_dir_all(&legacy).unwrap(); fs::write(legacy.join("old-thread.json"),raw()).unwrap();
        let migrated=migrate_legacy_goal_file(&reference,dir.path()).unwrap().unwrap(); assert_eq!(migrated.thread_id,"t"); assert_eq!(read_goal_file(&reference).unwrap(),Some(migrated));
    }
    #[test] fn upstream_no_session_legacy_conflict_is_not_guessed() {
        let dir=tempfile::tempdir().unwrap(); let key="c".repeat(24);
        let reference=GoalStoreRef { base_dir:dir.path().join("extensions/goal/no-session").join(&key),thread_id:"new-thread".into() };
        let legacy=dir.path().join("extensions/pi-goal/no-session").join(key); fs::create_dir_all(&legacy).unwrap();
        for name in ["one.json","two.json"] { fs::write(legacy.join(name),raw()).unwrap(); }
        assert!(matches!(migrate_legacy_goal_file(&reference,dir.path()),Err(GoalError::InvalidMutation(_)))); assert!(read_goal_file(&reference).unwrap().is_none());
    }
    #[test] fn upstream_migration_never_overwrites_current_inert_budget() {
        let dir=tempfile::tempdir().unwrap(); let reference=GoalStoreRef { base_dir:dir.path().join("extensions/goal"),thread_id:"t".into() };
        let mut current=parse_goal_file(&raw(),false).unwrap().goal.unwrap(); current.objective="current".into(); current.token_budget=Some(100);
        write_goal_file(&reference,Some(&current)).unwrap();
        let legacy=dir.path().join("extensions/pi-goal"); fs::create_dir_all(&legacy).unwrap(); fs::write(legacy.join("t.json"),raw()).unwrap();
        assert!(migrate_legacy_goal_file(&reference,dir.path()).unwrap().is_none()); assert_eq!(read_goal_file(&reference).unwrap(),Some(current));
    }
    #[test] fn upstream_absent_null_and_invalid_legacy_records_leave_store_usable() {
        for input in [None,Some("{\"version\":1,\"goal\":null}"),Some("{\"version\":99,\"goal\":null}"),Some("{\"version\":1,\"goal\":{}}"),Some("{\"version\":1,\"goal\":")] {
            let dir=tempfile::tempdir().unwrap(); let reference=GoalStoreRef { base_dir:dir.path().join("extensions/goal"),thread_id:"t".into() };
            if let Some(input)=input { let legacy=dir.path().join("extensions/pi-goal"); fs::create_dir_all(&legacy).unwrap(); fs::write(legacy.join("t.json"),input).unwrap(); }
            assert!(migrate_legacy_goal_file(&reference,dir.path()).unwrap().is_none()); assert!(read_goal_file(&reference).unwrap().is_none());
        }
    }
    #[test] fn upstream_legacy_snake_case_budget_status_becomes_active() {
        let mut input:Value=serde_json::from_str(&raw()).unwrap(); input["goal"]["status"]="budget_limited".into(); input["goal"]["tokenBudget"]=42.into();
        let goal=parse_goal_file(&input.to_string(),true).unwrap().goal.unwrap(); assert_eq!(goal.status,crate::types::GoalStatus::Active); assert!(goal.token_budget.is_none());
    }
    #[test] fn upstream_current_store_rejects_legacy_status_and_stale_brace_invalid_shape() {
        let mut input:Value=serde_json::from_str(&raw()).unwrap(); input["goal"]["status"]="budgetLimited".into();
        assert!(matches!(parse_goal_file(&input.to_string(),false),Err(GoalError::InvalidStore(_))));
        assert!(matches!(parse_goal_file("{\"version\":2,\"goal\":null}}",false),Err(GoalError::UnsupportedStoreVersion(_))));
        assert!(matches!(parse_goal_file("{\"version\":1,\"goal\":{}}}",false),Err(GoalError::InvalidStore(_))));
    }
    #[test] fn valid_record_roundtrips() { let input = raw(); let result = parse_goal_file(&input, false).unwrap(); assert_eq!(result.goal.unwrap().id, "g"); }
    #[test] fn saved_integral_elapsed_uses_javascript_json_number_spelling() {
        let mut goal=parse_goal_file(&raw(),false).unwrap().goal.unwrap(); goal.time_used_seconds=3.0;
        let text=contents(Some(&goal)).unwrap(); assert!(text.contains("\"timeUsedSeconds\": 3,")); assert!(!text.contains("3.0")); assert!(text.ends_with('\n'));
        assert_eq!(parse_goal_file(&text,false).unwrap().goal,Some(goal));
    }
    #[test] fn integral_floating_json_numbers_match_javascript_validation() {
        let input=r#"{"version":1.0,"goal":{"id":"g","threadId":"t","objective":"work","status":"blocked","tokensUsed":2.0,"timeUsedSeconds":3.0,"createdAt":1.0,"updatedAt":2e0,"blockedReason":"waiting","blockedAt":2.0,"consecutiveContinuations":4.0,"unattendedContinuations":5.0,"tokenBudget":6.0}}"#;
        let goal=parse_goal_file(input,false).unwrap().goal.unwrap();
        assert_eq!(goal.tokens_used,2); assert_eq!(goal.blocked_at,Some(2)); assert_eq!(goal.consecutive_continuations,Some(4)); assert_eq!(goal.unattended_continuations,Some(5)); assert_eq!(goal.token_budget,Some(6));
    }
    #[test] fn fractional_and_unsafe_persisted_counters_are_rejected() {
        for invalid in [1.5,-1.0,9007199254740992.0] {
            let mut value:Value=serde_json::from_str(&raw()).unwrap(); value["goal"]["tokensUsed"]=serde_json::json!(invalid);
            assert!(matches!(parse_goal_file(&value.to_string(),false),Err(GoalError::InvalidStore(_))));
        }
    }
    #[test] fn stale_closing_braces_recover() { let input = format!("{}}}\n}}\n", raw()); let result = parse_goal_file(&input, false).unwrap(); assert_eq!(result.goal.unwrap().objective, "work"); }
    #[test] fn truncated_json_is_rejected() { let input = "{\"version\":1,\"goal\":"; let result = parse_goal_file(input, false); assert!(matches!(result, Err(GoalError::Json(_)))); }
    #[test] fn arbitrary_suffix_is_rejected() { let input = format!("{}text", raw()); let result = parse_goal_file(&input, false); assert!(matches!(result, Err(GoalError::Json(_)))); }
    #[test] fn mismatched_container_is_rejected() { let input = "{\"version\":1,\"goal\":[}}}"; let result = parse_goal_file(input, false); assert!(matches!(result, Err(GoalError::Json(_)))); }
    #[test] fn inert_budget_is_preserved() { let mut input: Value = serde_json::from_str(&raw()).unwrap(); input["goal"]["tokenBudget"] = Value::from(8192); let result = parse_goal_file(&input.to_string(), false).unwrap(); assert_eq!(result.goal.unwrap().token_budget, Some(8192)); }
    #[test] fn invalid_continuation_state_is_sanitized() { let mut input: Value = serde_json::from_str(&raw()).unwrap(); input["goal"]["consecutiveContinuations"] = Value::from(-1); input["goal"]["lastContinuationSignature"] = Value::from(3); let result = parse_goal_file(&input.to_string(), false).unwrap().goal.unwrap(); assert_eq!(result.consecutive_continuations, None); assert_eq!(result.last_continuation_signature, None); }
    #[test] fn legacy_budget_status_resumes_without_budget() { let mut input: Value = serde_json::from_str(&raw()).unwrap(); input["goal"]["status"] = Value::from("budgetLimited"); input["goal"]["tokenBudget"] = Value::from(42); let result = parse_goal_file(&input.to_string(), true).unwrap().goal.unwrap(); assert_eq!(result.status, crate::types::GoalStatus::Active); assert_eq!(result.token_budget, None); }
    #[test] fn blocked_requires_reason_and_timestamp() { let mut input: Value = serde_json::from_str(&raw()).unwrap(); input["goal"]["status"] = Value::from("blocked"); let result = parse_goal_file(&input.to_string(), false); assert!(matches!(result, Err(GoalError::InvalidStore(_)))); }
    #[test] fn atomic_write_uses_private_permissions_and_cleans_temp() { use std::os::unix::fs::PermissionsExt; let dir = tempfile::tempdir().unwrap(); let reference = GoalStoreRef { base_dir: dir.path().join("goal"), thread_id: "t".into() }; let goal = parse_goal_file(&raw(), false).unwrap().goal; write_goal_file(&reference, goal.as_ref()).unwrap(); assert_eq!(read_goal_file(&reference).unwrap(), goal); assert_eq!(fs::metadata(goal_file_path(&reference)).unwrap().permissions().mode() & 0o777, 0o600); assert_eq!(fs::read_dir(&reference.base_dir).unwrap().count(), 1); }
    #[test] fn absent_file_returns_none() { let dir = tempfile::tempdir().unwrap(); let reference = GoalStoreRef { base_dir: dir.path().into(), thread_id: "missing".into() }; let result = read_goal_file(&reference).unwrap(); assert!(result.is_none()); }
    #[test] fn thread_id_is_uri_encoded() { let reference = GoalStoreRef { base_dir: PathBuf::new(), thread_id: "a/b 한".into() }; let result = encoded_thread_id(&reference); assert_eq!(result, "a%2Fb%20%ED%95%9C"); }
    #[test] fn legacy_session_file_migrates_once() { let dir = tempfile::tempdir().unwrap(); let reference = GoalStoreRef { base_dir: dir.path().join("extensions/goal"), thread_id: "t".into() }; let legacy = dir.path().join("extensions/pi-goal"); fs::create_dir_all(&legacy).unwrap(); fs::write(legacy.join("t.json"), raw()).unwrap(); let result = migrate_legacy_goal_file(&reference, dir.path()).unwrap(); assert!(result.is_some()); assert!(legacy.join("t.json.migrated").exists()); assert!(migrate_legacy_goal_file(&reference, dir.path()).unwrap().is_none()); }
}
