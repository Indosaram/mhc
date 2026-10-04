use std::path::Path;
use memory_core::{identity::resolve::MemoryIdentity, reflection::ReservedRun};
use super::{completion::{ReflectionCompletionRecord, CompletionDelivery, DeliveryStatus, ensure_reflection_completion, read_reflection_completion}, health::read_reflection_health, reservation_run_ledger::{ReservationRunLedger, parse_reservation_run_ledger}, run_artifacts::{read_run_json, update_run_ledger, write_run_json_atomic}, run_finalization_types::{DurableFinalizationDecision, ReservationRunResult}, runner_types::ReflectionReservationPort};

pub fn settle_reservation_run(identity: &MemoryIdentity, reservation: &dyn ReflectionReservationPort, run_dir: &Path, input: &ReservationRunLedger, decision: DurableFinalizationDecision, now_ms: i64, launch_notice: Option<&dyn Fn(&ReservedRun)>) -> Result<ReservationRunResult, String> {
    let path = run_dir.join("ledger.json");
    let current = parse_reservation_run_ledger(read_run_json(&path).map_err(|error| error.to_string())?).map_err(|error| error.to_string())?;
    if current.run_id() != input.run_id() { return Err("Finalization ledger run id changed".into()); }
    let value = current.value();
    let finalized = value["finalizedAt"].as_str().map(str::to_owned).unwrap_or_else(|| chrono::DateTime::from_timestamp_millis(now_ms).map(|time| time.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)).unwrap_or_default());
    let mut checkpoint = serde_json::Map::new();
    checkpoint.insert("finalizeOutcome".into(), decision.outcome.clone().into());
    checkpoint.insert("finalizedAt".into(), finalized.clone().into());
    for (key, field) in [("finalizeReason", &decision.reason), ("finalizeDetail", &decision.detail), ("integrationSha", &decision.integration_sha)] { if let Some(field) = field { checkpoint.insert(key.into(), field.clone().into()); } }
    update_run_ledger(&path, &checkpoint).map_err(|error| error.to_string())?;
    let active = reservation.read_state()?.active;
    let completions = identity.paths.reflection.join("completions");
    let existing = read_reflection_completion(&completions, current.run_id()).map_err(|error| error.to_string())?;
    let category = value["category"].as_str().map(str::to_owned).or_else(|| existing.as_ref().map(|record| record.category.clone()));
    let mut ids = value["conversationIds"].as_array().map(|ids| ids.iter().filter_map(serde_json::Value::as_str).map(str::to_owned).collect::<Vec<_>>()).or_else(|| active.as_ref().filter(|run| run.run_id == current.run_id()).map(|run| run.request.conversation_ids.clone())).or_else(|| existing.as_ref().map(|record| record.conversation_ids.clone()));
    if ids.is_none() {
        let legacy_path = run_dir.join("transcript-payload.json");
        if legacy_path.exists() {
            let legacy: serde_json::Value = read_run_json(&legacy_path).map_err(|error| error.to_string())?;
            ids = legacy["request"]["conversationIds"].as_array().filter(|ids| ids.iter().all(serde_json::Value::is_string)).map(|ids| ids.iter().filter_map(serde_json::Value::as_str).map(str::to_owned).collect());
        }
    }
    let (Some(category), Some(conversation_ids)) = (category, ids) else { return Err(format!("Reflection completion identity unavailable for {}", current.run_id())); };
    let launch = if active.as_ref().is_some_and(|run| run.run_id == current.run_id()) {
        let outcome = serde_json::from_value(decision.outcome.clone().into()).map_err(|error| error.to_string())?;
        let transition = reservation.complete(current.run_id(), outcome)?;
        if let (Some(run), Some(notice)) = (&transition.launch, launch_notice) { notice(run); }
        transition.launch
    } else { None };
    let health = read_reflection_health(&completions, 100, now_ms);
    let started = value["startedAt"].as_str().ok_or("Missing startedAt")?.to_owned();
    let duration = chrono::DateTime::parse_from_rfc3339(&finalized).ok().zip(chrono::DateTime::parse_from_rfc3339(&started).ok()).map(|(end, start)| (end.timestamp_millis() - start.timestamp_millis()).max(0) as f64);
    let completion = ensure_reflection_completion(&completions, &ReflectionCompletionRecord {
        schema_version:1, run_id:current.run_id().into(), identity:identity.id.clone(), category, model:value["model"].as_str().map(str::to_owned), thinking:value["thinking"].as_str().map(str::to_owned), conversation_ids,
        trigger:serde_json::from_value(value["trigger"].clone()).map_err(|error| error.to_string())?, origin:value.get("origin").map(|value| serde_json::from_value(value.clone())).transpose().map_err(|error| error.to_string())?,
        outcome:decision.outcome.clone(), reason:decision.reason.clone(), detail:decision.detail.clone(), started_at:started, finished_at:finalized.clone(), duration_ms:duration,
        merged_commit_sha:if decision.outcome == "merged" { decision.integration_sha.clone() } else { None }, files_changed:value["validatedChangedPaths"].as_array().map(Vec::len), consecutive_failures:Some(if decision.outcome == "failed" { health.streak + 1 } else { 0 }), delivery:CompletionDelivery { status:DeliveryStatus::Pending, session_id:None, consumed_at:None },
    }).map_err(|error| error.to_string())?;
    update_run_ledger(&path, serde_json::json!({"finalizePhase":"settled"}).as_object().ok_or("Invalid settlement checkpoint")?).map_err(|error| error.to_string())?;
    let mut terminal = serde_json::json!({"version":1,"runId":current.run_id(),"outcome":decision.outcome,"finishedAt":finalized});
    if let Some(sha) = decision.integration_sha { terminal["integrationSha"] = sha.into(); }
    write_run_json_atomic(&run_dir.join("final.json"), &terminal, 0o600).map_err(|error| error.to_string())?;
    Ok(ReservationRunResult { run_id:current.run_id().into(), outcome:decision.outcome, reason:decision.reason, detail:decision.detail, completion:Some(completion), launch })
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Inactive;
    impl ReflectionReservationPort for Inactive {
        fn read_state(&self) -> Result<memory_core::reflection::ReservationState, String> { Ok(Default::default()) }
        fn complete(&self, _: &str, _: memory_core::reflection::ReflectionOutcome) -> Result<memory_core::reflection::CompletionResult, String> { panic!("inactive reservation must not complete") }
    }
    #[test] fn settled_completion_and_terminal_are_idempotent() {
        let root = tempfile::tempdir().unwrap();
        let identity = MemoryIdentity { id:"agent".into(), safe_slug:"agent".into(), paths:memory_core::identity::layout::build_identity_paths(root.path(), "agent") };
        let value = serde_json::json!({"version":1,"runId":"run","kind":"reflection","trigger":"manual","startedAt":"2026-01-15T10:00:00.000Z","hardDeadlineAt":100,"terminationGraceMs":5,"deadlineAt":105,"mergePolicy":"auto","worktreeDir":"/unused","worktreeBranch":"memory/run","baseSha":"sha","gitFilePath":"/unused/.git","gitFileSnapshot":"gitdir: unused","commonConfigPath":"/unused/config","commonConfigSnapshot":null,"category":"quick","conversationIds":["conversation"],"validatedChangedPaths":[]});
        write_run_json_atomic(&root.path().join("ledger.json"), &value, 0o600).unwrap();
        let ledger = parse_reservation_run_ledger(value).unwrap();
        let decision = || DurableFinalizationDecision { outcome:"no_changes".into(), reason:None, detail:None, integration_sha:None };
        let result = settle_reservation_run(&identity, &Inactive, root.path(), &ledger, decision(), 1768471201000, None).unwrap();
        assert_eq!(result.completion.as_ref().unwrap().duration_ms, Some(1000.0));
        let again = settle_reservation_run(&identity, &Inactive, root.path(), &ledger, decision(), 1768471209000, None).unwrap();
        assert_eq!(again.completion.unwrap().finished_at, result.completion.unwrap().finished_at);
        let terminal: serde_json::Value = read_run_json(&root.path().join("final.json")).unwrap(); assert_eq!(terminal["outcome"], "no_changes");
        let checkpoint: serde_json::Value = read_run_json(&root.path().join("ledger.json")).unwrap(); assert_eq!(checkpoint["finalizePhase"], "settled");
    }
    #[test] fn missing_completion_identity_never_publishes_terminal() {
        let root = tempfile::tempdir().unwrap();
        let identity = MemoryIdentity { id:"agent".into(), safe_slug:"agent".into(), paths:memory_core::identity::layout::build_identity_paths(root.path(), "agent") };
        let value = serde_json::json!({"version":1,"runId":"run","kind":"reflection","trigger":"manual","startedAt":"now","hardDeadlineAt":100,"terminationGraceMs":5,"deadlineAt":105,"mergePolicy":"auto","worktreeDir":"/unused","worktreeBranch":"memory/run","baseSha":"sha","gitFilePath":"/unused/.git","gitFileSnapshot":"gitdir: unused","commonConfigPath":"/unused/config","commonConfigSnapshot":null});
        write_run_json_atomic(&root.path().join("ledger.json"), &value, 0o600).unwrap();
        let ledger = parse_reservation_run_ledger(value).unwrap();
        assert!(settle_reservation_run(&identity, &Inactive, root.path(), &ledger, DurableFinalizationDecision { outcome:"failed".into(), reason:None, detail:None, integration_sha:None }, 0, None).is_err());
        assert!(!root.path().join("final.json").exists());
    }
}
