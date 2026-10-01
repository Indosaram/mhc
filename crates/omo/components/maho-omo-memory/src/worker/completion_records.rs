use std::path::Path;
use memory_core::reflection::machine::ReflectionTrigger;
use super::{completion_contracts::ReflectionCompletionRecord, run_artifacts::{ArtifactError, read_run_json, write_run_json_atomic}};
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
pub fn read_completion_record(path: &Path) -> Option<ReflectionCompletionRecord> {
    let record: ReflectionCompletionRecord = read_run_json(path).ok()?;
    if record.schema_version != 1 || (record.trigger == ReflectionTrigger::Dream) != record.origin.is_some() { return None; }
    Some(record)
}
pub fn read_reflection_completion(dir: &Path, run_id: &str) -> Result<Option<ReflectionCompletionRecord>, CompletionRecordError> { Ok(read_completion_record(&dir.join(format!("{}.json", safe_run_id(run_id)?)))) }
pub fn write_completion_record(dir: &Path, record: &ReflectionCompletionRecord) -> Result<(), CompletionRecordError> {
    let mut builder = std::fs::DirBuilder::new(); builder.recursive(true);
    #[cfg(unix)] { use std::os::unix::fs::DirBuilderExt; builder.mode(0o700); }
    builder.create(dir).map_err(ArtifactError::Io)?;
    write_run_json_atomic(&dir.join(format!("{}.json", safe_run_id(&record.run_id)?)), record, 0o600)?;
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
    use crate::worker::completion_contracts::{CompletionDelivery, DeliveryStatus};
    fn record() -> ReflectionCompletionRecord { ReflectionCompletionRecord { schema_version: 1, run_id: "run-offline".into(), identity: "agent-test".into(), category: "quick".into(), model: None, thinking: None, conversation_ids: vec!["conversation-a".into()], trigger: ReflectionTrigger::Manual, origin: None, outcome: "merged".into(), reason: None, detail: None, started_at: "2026-08-16T11:59:00.000Z".into(), finished_at: "2026-08-16T11:59:00.000Z".into(), duration_ms: None, merged_commit_sha: None, files_changed: None, consecutive_failures: None, delivery: CompletionDelivery { status: DeliveryStatus::Pending, session_id: None, consumed_at: None } } }
    #[test]
    fn consumed_delivery_preserved() { let root = tempfile::tempdir().unwrap(); let mut consumed = record(); consumed.delivery.status = DeliveryStatus::Consumed; write_completion_record(root.path(), &consumed).unwrap(); assert_eq!(ensure_reflection_completion(root.path(), &record()).unwrap().delivery.status, DeliveryStatus::Consumed); }
    #[test]
    fn mismatch_rejected_without_overwrite() { let root = tempfile::tempdir().unwrap(); ensure_reflection_completion(root.path(), &record()).unwrap(); let mut bad = record(); bad.outcome = "failed".into(); assert!(ensure_reflection_completion(root.path(), &bad).is_err()); assert_eq!(read_reflection_completion(root.path(), "run-offline").unwrap().unwrap().outcome, "merged"); }
    #[test]
    fn offline_record_pending() { let root = tempfile::tempdir().unwrap(); assert_eq!(ensure_reflection_completion(root.path(), &record()).unwrap().delivery.status, DeliveryStatus::Pending); }
    #[test]
    fn corrupt_record_ignored() { let root = tempfile::tempdir().unwrap(); std::fs::write(root.path().join("bad.json"), "{").unwrap(); assert!(read_completion_record(&root.path().join("bad.json")).is_none()); }
}
