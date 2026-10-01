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
    if object.get("version").and_then(Value::as_u64) != Some(1) { return Err(GoalError::UnsupportedStoreVersion("unsupported goal store version".into())); }
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
    let goal = serde_json::from_value(value).map_err(|error| GoalError::InvalidStore(error.to_string()))?;
    Ok(GoalFile { version: 1, goal: Some(goal) })
}
fn contents(goal: Option<&Goal>) -> Result<String, GoalError> { serde_json::to_string_pretty(&GoalFile { version: 1, goal: goal.cloned() }).map(|text| format!("{text}\n")).map_err(|error| GoalError::Json(error.to_string())) }
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
fn retire_legacy(path: &Path) { if let Err(error) = fs::rename(path, format!("{}.migrated", path.display())) { eprintln!("goal legacy retirement failed: {error}"); } }
fn read_legacy_candidate(path: &Path) -> Result<Option<Goal>, GoalError> {
    let raw = match fs::read_to_string(path) { Ok(raw) => raw, Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None), Err(error) => return Err(GoalError::Io(error.to_string())) };
    match parse_goal_file(&raw, true) {
        Ok(file) => { if file.goal.is_none() { retire_legacy(path); } Ok(file.goal) },
        Err(GoalError::InvalidStore(_) | GoalError::UnsupportedStoreVersion(_) | GoalError::Json(_)) => Ok(None),
        Err(error) => Err(error),
    }
}
pub fn migrate_legacy_goal_file(reference: &GoalStoreRef, standalone_agent_dir: &Path) -> Result<Option<Goal>, GoalError> {
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
    paths.sort();
    let mut candidates = Vec::new();
    for path in paths { if let Some(goal) = read_legacy_candidate(&path)? { candidates.push((path, goal)); } }
    if candidates.len() > 1 { return Err(GoalError::InvalidMutation(format!("multiple legacy goals found for no-session store: {}", candidates.iter().map(|(p, _)| p.to_string_lossy()).collect::<Vec<_>>().join(", ")))); }
    let Some((path, goal)) = candidates.pop() else { return Ok(None); };
    fs::create_dir_all(&reference.base_dir).map_err(|error| GoalError::Io(error.to_string()))?;
    let published = match write_private(&goal_file_path(reference), &contents(Some(&goal))?, true) { Ok(()) => true, Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => false, Err(error) => return Err(GoalError::Io(error.to_string())) };
    retire_legacy(&path);
    Ok(published.then_some(goal))
}
#[cfg(test)] mod tests {
    use super::*;
    fn raw() -> String { serde_json::json!({"version":1,"goal":{"id":"g","threadId":"t","objective":"work","status":"active","tokensUsed":0,"timeUsedSeconds":0,"createdAt":1,"updatedAt":1}}).to_string() }
    #[test] fn valid_record_roundtrips() { let input = raw(); let result = parse_goal_file(&input, false).unwrap(); assert_eq!(result.goal.unwrap().id, "g"); }
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
