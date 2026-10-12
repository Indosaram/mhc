use std::path::Path;
use memory_core::reflection::machine::ReflectionTrigger;
use super::{completion_contracts::{CompletionDelivery, DeliveryStatus, ReflectionCompletionRecord}, run_artifacts::ArtifactError};
#[derive(Debug)]
pub enum CompletionRecordError { Artifact(ArtifactError), InvalidRunId, Mismatch(String) }
impl std::fmt::Display for CompletionRecordError { fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { match self { Self::Artifact(error) => error.fmt(f), Self::InvalidRunId => f.write_str("runId must contain a safe identifier"), Self::Mismatch(id) => write!(f, "Reflection completion record mismatch for {id}") } } }
impl std::error::Error for CompletionRecordError {}
impl From<ArtifactError> for CompletionRecordError { fn from(error: ArtifactError) -> Self { Self::Artifact(error) } }
pub fn safe_run_id(run_id: &str) -> Result<String, CompletionRecordError> {
    let name = Path::new(run_id.trim()).file_name().unwrap_or_default().to_string_lossy();
    let mut safe = String::new(); let mut in_invalid = false;
    for character in name.chars() {
        if character.is_ascii_alphanumeric() || "._-".contains(character) { safe.push(character); in_invalid = false; }
        else if !in_invalid { safe.push('-'); in_invalid = true; }
    }
    let safe = safe.trim_matches('-');
    if safe.is_empty() || safe == "." || safe == ".." { return Err(CompletionRecordError::InvalidRunId); }
    Ok(safe.chars().take(80).collect())
}
fn string_field(value: &serde_json::Map<String, serde_json::Value>, key: &str) -> Option<String> {
    value.get(key).and_then(serde_json::Value::as_str).map(str::to_owned)
}
/// Mirrors the TypeScript validator: required fields are type-checked, while the optional fields
/// the source never inspects (model, thinking, reason, detail, duration, merged metadata and the
/// delivery session fields) are read when well-typed and treated as absent otherwise instead of
/// discarding an otherwise valid completion.
pub fn read_completion_record(path: &Path) -> Option<ReflectionCompletionRecord> {
    let value: serde_json::Value = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    let value = value.as_object()?;
    if value.get("schemaVersion").and_then(serde_json::Value::as_f64) != Some(1.0) { return None; }
    let trigger: ReflectionTrigger = serde_json::from_value(value.get("trigger")?.clone()).ok()?;
    let origin: Option<memory_core::reflection::DreamOrigin> = match value.get("origin") {
        None => None,
        Some(value) => Some(serde_json::from_value(value.clone()).ok()?),
    };
    if (trigger == ReflectionTrigger::Dream) != origin.is_some() { return None; }
    let conversation_ids: Vec<String> = value.get("conversationIds")?.as_array()?.iter().map(|id| id.as_str().map(str::to_owned)).collect::<Option<Vec<_>>>()?;
    let delivery = value.get("delivery")?;
    let status = match delivery.get("status").and_then(serde_json::Value::as_str) {
        Some("pending") => DeliveryStatus::Pending,
        Some("consumed") => DeliveryStatus::Consumed,
        _ => return None,
    };
    Some(ReflectionCompletionRecord {
        schema_version: 1, run_id: string_field(value, "runId")?, identity: string_field(value, "identity")?,
        category: string_field(value, "category")?, model: string_field(value, "model"), thinking: string_field(value, "thinking"),
        conversation_ids, trigger, origin, outcome: string_field(value, "outcome")?,
        reason: string_field(value, "reason"), detail: string_field(value, "detail"),
        started_at: string_field(value, "startedAt")?, finished_at: string_field(value, "finishedAt")?,
        duration_ms: match value.get("durationMs") {
            // The source writes a non-finite duration as JSON null; `null` then flows into
            // `formatDuration` and renders "unknown" rather than reading back as absent.
            None => None,
            Some(serde_json::Value::Null) => Some(f64::NAN),
            Some(duration) => duration.as_f64(),
        },
        merged_commit_sha: string_field(value, "mergedCommitSha"),
        files_changed: value.get("filesChanged").and_then(serde_json::Value::as_u64).and_then(|count| usize::try_from(count).ok()),
        consecutive_failures: value.get("consecutiveFailures").and_then(serde_json::Value::as_u64).and_then(|count| usize::try_from(count).ok()),
        delivery: CompletionDelivery { status, session_id: delivery.get("sessionId").and_then(serde_json::Value::as_str).map(str::to_owned), consumed_at: delivery.get("consumedAt").and_then(serde_json::Value::as_str).map(str::to_owned) },
    })
}
pub fn read_reflection_completion(dir: &Path, run_id: &str) -> Result<Option<ReflectionCompletionRecord>, CompletionRecordError> { Ok(read_completion_record(&dir.join(format!("{}.json", safe_run_id(run_id)?)))) }
pub fn write_completion_record(dir: &Path, record: &ReflectionCompletionRecord) -> Result<(), CompletionRecordError> {
    let mut builder = std::fs::DirBuilder::new(); builder.recursive(true);
    #[cfg(unix)] { use std::os::unix::fs::DirBuilderExt; builder.mode(0o700); }
    builder.create(dir).map_err(ArtifactError::Io)?;
    let target=dir.join(format!("{}.json",safe_run_id(&record.run_id)?));
    let temporary=target.with_file_name(format!("{}.tmp-{}",target.file_name().unwrap_or_default().to_string_lossy(),memory_core::support::random::random_uuid()));
    let mut options=std::fs::OpenOptions::new();options.write(true).create(true).truncate(true);
    #[cfg(unix)]{use std::os::unix::fs::OpenOptionsExt;options.mode(0o600);}
    {use std::io::Write;let mut file=options.open(&temporary).map_err(ArtifactError::Io)?;file.write_all(serde_json::to_string_pretty(record).map_err(ArtifactError::Json)?.as_bytes()).map_err(ArtifactError::Io)?;file.write_all(b"\n").map_err(ArtifactError::Io)?;}
    std::fs::rename(temporary,target).map_err(ArtifactError::Io)?;
    Ok(())
}
pub fn ensure_reflection_completion(dir: &Path, desired: &ReflectionCompletionRecord) -> Result<ReflectionCompletionRecord, CompletionRecordError> {
    if let Some(existing) = read_reflection_completion(dir, &desired.run_id)? {
        let mut left = serde_json::to_value(&existing).map_err(ArtifactError::Json)?;
        let mut right = serde_json::to_value(desired).map_err(ArtifactError::Json)?;
        if let Some(value) = left.as_object_mut() { value.remove("delivery"); }
        if let Some(value) = right.as_object_mut() { value.remove("delivery"); }
        if left != right { return Err(CompletionRecordError::Mismatch(desired.run_id.clone())); }
        return Ok(existing);
    }
    write_completion_record(dir, desired)?;
    Ok(desired.clone())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mistyped_optional_fields_stay_readable() {
        let root = tempfile::tempdir().unwrap(); let path = root.path().join("mixed.json");
        std::fs::write(&path, serde_json::to_vec(&serde_json::json!({"schemaVersion":1.0,"runId":"run-mixed","identity":"agent-test","category":"quick","conversationIds":["conversation-a"],"trigger":"manual","outcome":"merged","model":7,"thinking":["x"],"reason":false,"startedAt":"2026-08-16T11:59:00.000Z","finishedAt":"2026-08-16T11:59:30.000Z","durationMs":"slow","filesChanged":2,"delivery":{"status":"pending","sessionId":5}})).unwrap()).unwrap();
        let record = read_completion_record(&path).unwrap();
        assert_eq!(record.run_id, "run-mixed"); assert_eq!(record.finished_at, "2026-08-16T11:59:30.000Z");
        assert_eq!(record.model, None); assert_eq!(record.reason, None); assert_eq!(record.duration_ms, None);
        let null_duration = root.path().join("null-duration.json");
        std::fs::write(&null_duration, serde_json::to_vec(&serde_json::json!({"schemaVersion":1,"runId":"run-null","identity":"agent-test","category":"quick","conversationIds":[],"trigger":"manual","outcome":"merged","startedAt":"2026-08-16T11:59:00.000Z","finishedAt":"2026-08-16T11:59:30.000Z","durationMs":null,"delivery":{"status":"pending"}})).unwrap()).unwrap();
        assert!(read_completion_record(&null_duration).unwrap().duration_ms.expect("null reads back as a NaN duration").is_nan());
        assert_eq!(record.files_changed, Some(2)); assert_eq!(record.delivery.status, DeliveryStatus::Pending); assert_eq!(record.delivery.session_id, None);
    }
    #[test]
    fn missing_required_field_and_dream_origin_are_rejected() {
        let root = tempfile::tempdir().unwrap();
        let partial = root.path().join("partial.json");
        std::fs::write(&partial, serde_json::to_vec(&serde_json::json!({"schemaVersion":1,"runId":"run-x","identity":"agent","category":"quick","conversationIds":[],"trigger":"manual","outcome":"merged","startedAt":"2026-08-16T11:59:00.000Z","delivery":{"status":"pending"}})).unwrap()).unwrap();
        assert!(read_completion_record(&partial).is_none());
        let dream = root.path().join("dream.json");
        std::fs::write(&dream, serde_json::to_vec(&serde_json::json!({"schemaVersion":1,"runId":"run-d","identity":"agent","category":"quick","conversationIds":[],"trigger":"dream","outcome":"merged","startedAt":"2026-08-16T11:59:00.000Z","finishedAt":"2026-08-16T11:59:00.000Z","delivery":{"status":"pending"}})).unwrap()).unwrap();
        assert!(read_completion_record(&dream).is_none());
        std::fs::write(&dream, serde_json::to_vec(&serde_json::json!({"schemaVersion":1,"runId":"run-d","identity":"agent","category":"quick","conversationIds":[],"trigger":"manual","origin":"idle","outcome":"merged","startedAt":"2026-08-16T11:59:00.000Z","finishedAt":"2026-08-16T11:59:00.000Z","delivery":{"status":"pending"}})).unwrap()).unwrap();
        assert!(read_completion_record(&dream).is_none());
    }
    use crate::worker::completion_contracts::{CompletionDelivery, DeliveryStatus};
    fn record() -> ReflectionCompletionRecord { ReflectionCompletionRecord { schema_version: 1, run_id: "run-offline".into(), identity: "agent-test".into(), category: "quick".into(), model: None, thinking: None, conversation_ids: vec!["conversation-a".into()], trigger: ReflectionTrigger::Manual, origin: None, outcome: "merged".into(), reason: None, detail: None, started_at: "2026-08-16T11:59:00.000Z".into(), finished_at: "2026-08-16T11:59:00.000Z".into(), duration_ms: None, merged_commit_sha: None, files_changed: None, consecutive_failures: None, delivery: CompletionDelivery { status: DeliveryStatus::Pending, session_id: None, consumed_at: None } } }
    #[test]
    fn consumed_delivery_preserved() { let root = tempfile::tempdir().unwrap(); let mut consumed = record(); consumed.delivery.status = DeliveryStatus::Consumed; write_completion_record(root.path(), &consumed).unwrap(); assert_eq!(ensure_reflection_completion(root.path(), &record()).unwrap().delivery.status, DeliveryStatus::Consumed); }
    #[test]
    fn mismatch_rejected_without_overwrite() { let root = tempfile::tempdir().unwrap(); ensure_reflection_completion(root.path(), &record()).unwrap(); let mut bad = record(); bad.outcome = "failed".into(); assert!(ensure_reflection_completion(root.path(), &bad).is_err()); assert_eq!(read_reflection_completion(root.path(), "run-offline").unwrap().unwrap().outcome, "merged"); }
    #[test]
    fn offline_record_pending() { let root = tempfile::tempdir().unwrap();let expected=record(); assert_eq!(ensure_reflection_completion(root.path(), &expected).unwrap().delivery.status, DeliveryStatus::Pending);assert_eq!(std::fs::read(root.path().join("run-offline.json")).unwrap(),format!("{}\n",serde_json::to_string_pretty(&expected).unwrap()).as_bytes());assert_eq!(std::fs::read_dir(root.path()).unwrap().count(),1); }
    #[test]
    fn corrupt_record_ignored() { let root = tempfile::tempdir().unwrap(); std::fs::write(root.path().join("bad.json"), "{").unwrap(); assert!(read_completion_record(&root.path().join("bad.json")).is_none()); }
}
