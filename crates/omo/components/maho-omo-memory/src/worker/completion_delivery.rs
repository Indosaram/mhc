use std::{collections::BTreeMap, path::Path};
use super::{completion_contracts::*, completion_records::{CompletionRecordError, read_completion_record, write_completion_record}, run_artifacts::ArtifactError};
pub trait ReflectionLiveSession {
    fn session_id(&self) -> &str;
    fn append_entry(&mut self, custom_type: &str, data: serde_json::Value);
    fn notify(&mut self, message: &str, warning: bool) -> Result<(), String>;
    fn warn(&mut self, message: &str, error: &str);
    fn on_completion(&mut self, run_id: &str);
}
fn parsed_time(value: &str) -> Option<i64> { chrono::DateTime::parse_from_rfc3339(value).ok().map(|time| time.timestamp_millis()) }
fn consumed_timestamp(now_ms: i64) -> String { chrono::DateTime::from_timestamp_millis(now_ms).map(|time| time.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)).unwrap_or_default() }
fn mark_delivered(dir: &Path, record: &ReflectionCompletionRecord, session_id: &str, now_ms: i64) -> Result<ReflectionCompletionRecord, CompletionRecordError> {
    let mut delivered = record.clone(); delivered.delivery = CompletionDelivery { status: DeliveryStatus::Consumed, session_id: Some(session_id.to_owned()), consumed_at: Some(consumed_timestamp(now_ms)) };
    write_completion_record(dir, &delivered)?; Ok(delivered)
}
pub fn safe_notify(live: &mut dyn ReflectionLiveSession, message: &str, warning: bool) {
    if let Err(error) = live.notify(message, warning) { live.warn("memory reflection notification failed", &error); }
}
fn unsuccessful(record: &ReflectionCompletionRecord) -> bool { record.outcome != "merged" && record.outcome != "no_changes" }
pub fn deliver_reflection_completion(dir: &Path, record: &ReflectionCompletionRecord, live: &mut dyn ReflectionLiveSession, notify: bool, now_ms: i64) -> Result<ReflectionCompletionRecord, CompletionRecordError> {
    let delivered = mark_delivered(dir, record, live.session_id(), now_ms)?;
    live.append_entry(REFLECTION_COMPLETION_ENTRY_TYPE, serde_json::to_value(&delivered).map_err(ArtifactError::Json)?);
    if notify {
        let message = match delivered.outcome.as_str() {
            "merged" => format!("Memory reflection {} merged.", delivered.run_id),
            "no_changes" => format!("Memory reflection {} completed with no changes.", delivered.run_id),
            "timed_out" => format!("Memory reflection {} timed out; its transcript cursor was not advanced.", delivered.run_id),
            outcome => format!("Memory reflection {} ended with {outcome}; its transcript cursor was not advanced.", delivered.run_id),
        };
        safe_notify(live, &message, unsuccessful(&delivered));
    }
    Ok(delivered)
}
pub fn consume_pending_reflection_completions(dir: &Path, identity: &str, live: &mut dyn ReflectionLiveSession, now_ms: i64) -> Result<Vec<ReflectionCompletionRecord>, CompletionRecordError> {
    let entries = match std::fs::read_dir(dir) { Ok(entries) => entries, Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()), Err(error) => return Err(ArtifactError::Io(error).into()) };
    let mut names = Vec::new();
    for entry in entries { let entry = entry.map_err(ArtifactError::Io)?; if entry.path().extension().is_some_and(|ext| ext == "json") { names.push(entry.path()); } }
    names.sort();
    let mut pending: Vec<_> = names.iter().filter_map(|path| read_completion_record(path)).filter(|record| record.identity == identity && record.delivery.status == DeliveryStatus::Pending).collect();
    pending.sort_by(|left, right| match (parsed_time(&right.finished_at), parsed_time(&left.finished_at)) { (Some(right), Some(left)) => right.cmp(&left), _ => std::cmp::Ordering::Equal });
    let cutoff = now_ms.saturating_sub(7 * 24 * 60 * 60_000);
    let fresh: Vec<_> = pending.iter().filter(|record| parsed_time(&record.finished_at).is_some_and(|time| time >= cutoff)).collect();
    let stale: Vec<_> = pending.iter().filter(|record| parsed_time(&record.finished_at).is_some_and(|time| time < cutoff)).collect();
    let mut consumed = Vec::new();
    for record in fresh.iter().take(5) { consumed.push(deliver_reflection_completion(dir, record, live, false, now_ms)?); }
    let collapsed = fresh.get(5..).unwrap_or_default();
    if !collapsed.is_empty() {
        let mut fingerprints = BTreeMap::new();
        for record in collapsed {
            let detail = String::from_utf16_lossy(&record.detail.as_deref().unwrap_or("").encode_utf16().take(60).collect::<Vec<_>>());
            *fingerprints.entry(format!("{}:{detail}", record.reason.as_deref().unwrap_or(&record.outcome))).or_insert(0usize) += 1;
        }
        let summary = ReflectionCompletionSummary { schema_version: 1, count: collapsed.len(), failed_count: collapsed.iter().filter(|record| unsuccessful(record)).count(), oldest_iso: collapsed.last().map(|record| record.finished_at.clone()).unwrap_or_default(), newest_iso: collapsed.first().map(|record| record.finished_at.clone()).unwrap_or_default(), dominant_fingerprint: fingerprints.into_iter().max_by(|(left,a),(right,b)| a.cmp(b).then_with(|| right.cmp(left))).map(|(key,_)| key).unwrap_or_default() };
        live.append_entry(REFLECTION_SUMMARY_ENTRY_TYPE, serde_json::to_value(summary).map_err(ArtifactError::Json)?);
        for record in collapsed { consumed.push(mark_delivered(dir, record, live.session_id(), now_ms)?); }
    }
    for record in stale { consumed.push(mark_delivered(dir, record, live.session_id(), now_ms)?); }
    if !fresh.is_empty() {
        let failures = fresh.iter().filter(|record| unsuccessful(record)).count();
        let message = if failures == 0 { format!("Delivered {} memory reflection completion{}.", fresh.len(), if fresh.len() == 1 { "" } else { "s" }) } else { format!("Delivered {} memory reflection completions; {failures} need attention.", fresh.len()) };
        safe_notify(live, &message, failures != 0);
    }
    Ok(consumed)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Default)]
    struct Live { entries: Vec<(String, serde_json::Value)>, notifications: usize, warnings: usize, fail_notify: bool }
    impl ReflectionLiveSession for Live {
        fn session_id(&self) -> &str { "new-session" }
        fn append_entry(&mut self, kind: &str, data: serde_json::Value) { self.entries.push((kind.into(),data)); }
        fn notify(&mut self, _: &str, _: bool) -> Result<(), String> { self.notifications += 1; if self.fail_notify { Err("ui unavailable".into()) } else { Ok(()) } }
        fn warn(&mut self, _: &str, _: &str) { self.warnings += 1; }
        fn on_completion(&mut self, _: &str) {}
    }
    fn record(id: &str, identity: &str, finished_at: &str) -> ReflectionCompletionRecord {
        serde_json::from_value(serde_json::json!({"schemaVersion":1,"runId":id,"identity":identity,"category":"quick","conversationIds":["old-session"],"trigger":"manual","outcome":"merged","startedAt":finished_at,"finishedAt":finished_at,"delivery":{"status":"pending"}})).unwrap()
    }
    #[test]
    fn offline_completion_delivers_without_model_message() { let root = tempfile::tempdir().unwrap(); write_completion_record(root.path(), &record("run-1","agent-test","2026-08-16T11:59:00.000Z")).unwrap(); let mut live = Live::default(); let consumed = consume_pending_reflection_completions(root.path(),"agent-test",&mut live,parsed_time("2026-08-16T12:00:00.000Z").unwrap()).unwrap(); assert_eq!(consumed.len(),1); assert_eq!(live.entries.len(),1); assert_eq!(live.entries[0].0, REFLECTION_COMPLETION_ENTRY_TYPE); assert_eq!(live.notifications,1); }
    #[test]
    fn foreign_identity_unconsumed() { let root = tempfile::tempdir().unwrap(); for index in 0..8 { write_completion_record(root.path(), &record(&format!("target-{index}"),"agent-test","2026-08-16T11:59:00.000Z")).unwrap(); } write_completion_record(root.path(), &record("foreign","other","2026-08-16T11:59:00.000Z")).unwrap(); let mut live = Live::default(); consume_pending_reflection_completions(root.path(),"agent-test",&mut live,parsed_time("2026-08-16T12:00:00.000Z").unwrap()).unwrap(); assert_eq!(live.entries.len(),6); assert_eq!(live.entries.last().unwrap().1["count"],3); assert_eq!(read_completion_record(&root.path().join("foreign.json")).unwrap().delivery.status,DeliveryStatus::Pending); }
    #[test]
    fn throwing_ui_is_logged_after_consumption() { let root = tempfile::tempdir().unwrap(); write_completion_record(root.path(), &record("run-1","agent-test","2026-08-16T11:59:00.000Z")).unwrap(); let mut live = Live { fail_notify: true, ..Default::default() }; let consumed = consume_pending_reflection_completions(root.path(),"agent-test",&mut live,parsed_time("2026-08-16T12:00:00.000Z").unwrap()).unwrap(); assert_eq!(consumed[0].delivery.status,DeliveryStatus::Consumed); assert_eq!(live.warnings,1); }
    #[test]
    fn stale_records_silent_and_drain_once() { let root = tempfile::tempdir().unwrap(); for index in 0..8 { write_completion_record(root.path(), &record(&format!("run-{index}"),"agent-test",if index < 6 { "2026-08-16T11:59:00.000Z" } else { "2026-08-01T11:59:00.000Z" })).unwrap(); } let mut live = Live::default(); let now = parsed_time("2026-08-16T12:00:00.000Z").unwrap(); assert_eq!(consume_pending_reflection_completions(root.path(),"agent-test",&mut live,now).unwrap().len(),8); assert!(consume_pending_reflection_completions(root.path(),"agent-test",&mut live,now).unwrap().is_empty()); assert_eq!(live.entries.len(),6); assert_eq!(live.entries.last().unwrap().1["count"],1); assert_eq!(live.notifications,1); }
}
