use std::{collections::BTreeMap, path::Path};
use memory_core::reflection::machine::ReflectionOutcome;
use serde::Deserialize;

pub const REFLECTION_HEALTH_STALE_MS: i64 = 7 * 24 * 60 * 60_000;
#[derive(Clone, Debug)]
pub struct LastFailure { pub reason: String, pub detail: Option<String>, pub finished_at: String }
#[derive(Clone, Debug)]
pub struct LastOutcome { pub run_id: String, pub outcome: ReflectionOutcome, pub reason: Option<String>, pub finished_at: String }
#[derive(Clone, Debug, Default)]
pub struct HealthCounts { pub merged: usize, pub no_changes: usize, pub failed: usize, pub timed_out: usize }
#[derive(Clone, Debug, Default)]
pub struct ReflectionHealth {
    pub streak: usize, pub fingerprint: String, pub last_failure: Option<LastFailure>,
    pub last_success_at: Option<String>, pub last_outcome: Option<LastOutcome>,
    pub counts: HealthCounts, pub pending_count: usize, pub recent_failure_fingerprints: Vec<String>,
    pub streak_since_iso: Option<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct HealthRecord {
    run_id: Option<String>, outcome: ReflectionOutcome, reason: Option<String>, detail: Option<String>,
    finished_at: String, delivery: Option<serde_json::Value>,
}
fn timestamp(value: &str) -> Option<i64> { chrono::DateTime::parse_from_rfc3339(value).ok().map(|time| time.timestamp_millis()) }
pub fn reflection_failure_fingerprint(reason: Option<&str>, detail: Option<&str>) -> String {
    let excerpt = String::from_utf16_lossy(&detail.unwrap_or("").encode_utf16().take(60).collect::<Vec<_>>());
    format!("{}:{excerpt}", reason.unwrap_or("failed"))
}
pub fn read_reflection_health(completions_dir: &Path, limit: usize, now: i64) -> ReflectionHealth {
    let Ok(names) = std::fs::read_dir(completions_dir) else { return ReflectionHealth::default(); };
    let mut records: Vec<HealthRecord> = names.filter_map(Result::ok).filter(|entry| entry.path().extension().is_some_and(|extension| extension == "json"))
        .filter_map(|entry| std::fs::read(entry.path()).ok()).filter_map(|bytes| serde_json::from_slice(&bytes).ok()).collect();
    records.sort_by(|left, right| match (timestamp(&right.finished_at), timestamp(&left.finished_at)) { (Some(right), Some(left)) => right.cmp(&left), _ => std::cmp::Ordering::Equal });
    records.truncate(limit);
    let mut health = ReflectionHealth::default();
    for record in &records {
        match record.outcome {
            ReflectionOutcome::Merged => health.counts.merged += 1,
            ReflectionOutcome::NoChanges => health.counts.no_changes += 1,
            ReflectionOutcome::Failed => health.counts.failed += 1,
            ReflectionOutcome::TimedOut => health.counts.timed_out += 1,
            ReflectionOutcome::ParentDirty | ReflectionOutcome::MergeConflict | ReflectionOutcome::DirtyUncommitted => {}
        }
        if record.delivery.as_ref().and_then(|value| value.get("status")).and_then(serde_json::Value::as_str) == Some("pending") { health.pending_count += 1; }
    }
    let failures: Vec<&HealthRecord> = records.iter().take_while(|record| !matches!(record.outcome, ReflectionOutcome::Merged | ReflectionOutcome::NoChanges)).filter(|record| record.outcome == ReflectionOutcome::Failed).collect();
    let stale = failures.first().and_then(|record| timestamp(&record.finished_at)).is_some_and(|time| now.saturating_sub(time) > REFLECTION_HEALTH_STALE_MS);
    if !stale {
        health.streak = failures.len();
        health.streak_since_iso = failures.last().map(|record| record.finished_at.clone());
        health.recent_failure_fingerprints = failures.iter().take(3).map(|record| reflection_failure_fingerprint(record.reason.as_deref(), record.detail.as_deref())).collect();
        let mut counts = BTreeMap::new();
        for fingerprint in &health.recent_failure_fingerprints { *counts.entry(fingerprint.clone()).or_insert(0usize) += 1; }
        health.fingerprint = counts.into_iter().max_by(|(left, a), (right, b)| a.cmp(b).then_with(|| right.cmp(left))).map(|(fingerprint, _)| fingerprint).unwrap_or_default();
    }
    health.last_failure = records.iter().find(|record| record.outcome == ReflectionOutcome::Failed).map(|record| LastFailure { reason: record.reason.clone().unwrap_or_else(|| "failed".into()), detail: record.detail.clone(), finished_at: record.finished_at.clone() });
    health.last_success_at = records.iter().find(|record| matches!(record.outcome, ReflectionOutcome::Merged | ReflectionOutcome::NoChanges)).map(|record| record.finished_at.clone());
    health.last_outcome = records.first().map(|record| LastOutcome { run_id: record.run_id.clone().unwrap_or_default(), outcome: record.outcome, reason: record.reason.clone(), finished_at: record.finished_at.clone() });
    health
}

#[cfg(test)]
mod tests {
    use super::*;
    fn write(root: &Path, id: &str, finished: &str, outcome: &str, detail: &str, pending: bool) {
        std::fs::write(root.join(format!("{id}.json")), serde_json::to_vec(&serde_json::json!({"runId": id, "outcome": outcome, "reason": "child_exit", "detail": detail, "finishedAt": finished, "delivery": {"status": if pending {"pending"} else {"consumed"}}})).unwrap()).unwrap();
    }
    #[test]
    fn unordered_and_corrupt_records_derive_streak() {
        let root = tempfile::tempdir().unwrap();
        write(root.path(), "success", "2026-08-12T00:00:00.000Z", "merged", "", false);
        for hour in 1..=4 { write(root.path(), &format!("failure-{hour}"), &format!("2026-08-12T0{hour}:00:00.000Z"), "failed", if hour == 3 {"different"} else {"same"}, hour == 1); }
        write(root.path(), "timeout", "2026-08-11T23:00:00.000Z", "timed_out", "slow", false);
        std::fs::write(root.path().join("corrupt.json"), "{").unwrap();
        let health = read_reflection_health(root.path(), 100, timestamp("2026-08-12T05:00:00.000Z").unwrap());
        assert_eq!(health.streak, 4); assert_eq!(health.fingerprint, "child_exit:same"); assert_eq!(health.counts.failed, 4); assert_eq!(health.counts.merged, 1); assert_eq!(health.counts.timed_out, 1); assert_eq!(health.pending_count, 1); assert_eq!(health.last_outcome.unwrap().run_id, "failure-4");
    }
    #[test]
    fn derivation_leaves_bytes_unchanged() {
        let root = tempfile::tempdir().unwrap(); write(root.path(), "one", "2026-08-12T00:00:00.000Z", "failed", "stable", false);
        let before = std::fs::read(root.path().join("one.json")).unwrap(); read_reflection_health(root.path(), 100, 0);
        assert_eq!(std::fs::read(root.path().join("one.json")).unwrap(), before); assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1);
    }
    #[test]
    fn no_writes_even_for_alert_threshold() { let root = tempfile::tempdir().unwrap(); for hour in 0..3 { write(root.path(), &format!("{hour}"), &format!("2026-08-12T0{hour}:00:00.000Z"), "failed", "stable", false); } assert_eq!(read_reflection_health(root.path(), 100, 0).streak, 3); assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 3); }
    #[test]
    fn newer_success_breaks_streak() { let root = tempfile::tempdir().unwrap(); write(root.path(), "old", "2026-08-12T01:00:00.000Z", "failed", "boom", false); write(root.path(), "new", "2026-08-12T02:00:00.000Z", "merged", "", false); let health = read_reflection_health(root.path(), 100, 0); assert_eq!(health.streak, 0); assert_eq!(health.last_outcome.unwrap().run_id, "new"); }
    #[test]
    fn missing_directory_zeroes_health() { let root = tempfile::tempdir().unwrap(); assert_eq!(read_reflection_health(&root.path().join("missing"), 100, 0).streak, 0); }
    #[test]
    fn stale_failures_keep_history_not_streak() { let root = tempfile::tempdir().unwrap(); write(root.path(), "old", "2026-08-08T00:00:00.000Z", "failed", "stable", false); let health = read_reflection_health(root.path(), 100, timestamp("2026-08-17T00:00:00.000Z").unwrap()); assert_eq!(health.streak, 0); assert!(health.fingerprint.is_empty()); assert_eq!(health.counts.failed, 1); assert!(health.last_failure.is_some()); }
    #[test]
    fn exact_stale_boundary_counts() { let root = tempfile::tempdir().unwrap(); write(root.path(), "edge", "2026-08-10T00:00:00.000Z", "failed", "stable", false); assert_eq!(read_reflection_health(root.path(), 100, timestamp("2026-08-17T00:00:00.000Z").unwrap()).streak, 1); }
    #[test]
    fn fresh_failure_resumes_entire_streak() { let root = tempfile::tempdir().unwrap(); write(root.path(), "old", "2026-08-08T00:00:00.000Z", "failed", "stable", false); write(root.path(), "fresh", "2026-08-17T00:00:00.000Z", "failed", "stable", false); assert_eq!(read_reflection_health(root.path(), 100, timestamp("2026-08-17T12:00:00.000Z").unwrap()).streak, 2); }
}
