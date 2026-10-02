use std::path::Path;
use super::{run_artifacts::{RunOutcome, read_run_json, run_outcome_matches_ledger}, reservation_run_ledger::{ReservationRunLedger, parse_reservation_run_ledger}, run_finalization_claim::{ClaimedRunResult, with_run_finalization_claim}, run_finalization_git::resolve_finalization_decision, run_finalization_settlement::settle_reservation_run, run_finalization_types::{ReservationRunResult, RunFinalizationContext}};

pub fn finalize_recorded_outcome(context: &RunFinalizationContext<'_>, run_dir: &Path, input: &ReservationRunLedger) -> Result<Option<ReservationRunResult>, String> {
    let claimed = with_run_finalization_claim(&context.identity.paths.locks, run_dir, input.run_id(), || {
        let ledger = parse_reservation_run_ledger(read_run_json(&run_dir.join("ledger.json")).map_err(|error| error.to_string())?).map_err(|error| error.to_string())?;
        if ledger.run_id() != input.run_id() { return Err(format!("Finalization ledger mismatch: {}", input.run_id())); }
        let outcome: RunOutcome = read_run_json(&run_dir.join("outcome.json")).map_err(|error| error.to_string())?;
        let attempt = ledger.value()["attempt"].as_u64().map(|value| u32::try_from(value).map_err(|error| error.to_string())).transpose()?;
        if !run_outcome_matches_ledger(attempt, &outcome) { return Err(format!("Run outcome attempt {} does not match {}", outcome.attempt.map_or("legacy".into(), |value| value.to_string()), attempt.map_or("legacy".into(), |value| value.to_string()))); }
        let decision = resolve_finalization_decision(context.identity, run_dir, &ledger, &outcome)?;
        settle_reservation_run(context.identity, context.reservation, run_dir, &ledger, decision, (context.now_ms)(), context.launch)
    })?;
    Ok(match claimed {
        ClaimedRunResult::Completed(result) => Some(result),
        ClaimedRunResult::Terminal(result) => Some(ReservationRunResult { run_id:result.run_id, outcome:result.outcome, reason:None, detail:None, completion:None, launch:None }),
        ClaimedRunResult::Busy => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn terminal_sentinel_returns_without_reservation_or_git_work() {
        struct Reservation;
        impl super::super::runner_types::ReflectionReservationPort for Reservation {
            fn read_state(&self) -> Result<memory_core::reflection::ReservationState, String> { panic!("terminal reservation must not be read") }
            fn complete(&self, _: &str, _: memory_core::reflection::ReflectionOutcome) -> Result<memory_core::reflection::CompletionResult, String> { panic!("terminal reservation must not be completed") }
        }
        let root = tempfile::tempdir().unwrap();
        let identity = memory_core::identity::resolve::MemoryIdentity { id:"agent".into(), safe_slug:"agent".into(), paths:memory_core::identity::layout::build_identity_paths(root.path(), "agent") };
        let ledger = parse_reservation_run_ledger(serde_json::json!({"version":1,"runId":"run","kind":"reflection","trigger":"manual","startedAt":"now","hardDeadlineAt":100,"terminationGraceMs":5,"deadlineAt":105,"mergePolicy":"auto","worktreeDir":"/unused","worktreeBranch":"memory/run","baseSha":"sha","gitFilePath":"/unused/.git","gitFileSnapshot":"gitdir: unused","commonConfigPath":"/unused/config","commonConfigSnapshot":null})).unwrap();
        super::super::run_artifacts::write_run_json_atomic(&root.path().join("final.json"), &serde_json::json!({"runId":"run","outcome":"merged"}), 0o600).unwrap();
        let result = finalize_recorded_outcome(&RunFinalizationContext { identity:&identity, reservation:&Reservation, launch:None, now_ms:&||0 }, root.path(), &ledger).unwrap().unwrap();
        assert_eq!(result.outcome, "merged"); assert!(result.completion.is_none());
    }
}
