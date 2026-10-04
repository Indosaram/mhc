use std::path::Path;
use super::{run_artifacts::{RunOutcome, read_run_json, run_outcome_matches_ledger}, reservation_run_ledger::{ReservationRunLedger, parse_reservation_run_ledger}, run_finalization_claim::{ClaimedRunResult, with_run_finalization_claim}, run_finalization_git::resolve_finalization_decision, run_finalization_settlement::settle_reservation_run, run_finalization_types::{ReservationRunResult, RunFinalizationContext}};

pub fn fail_reservation_run(context:&RunFinalizationContext<'_>,run_dir:&Path,input:&ReservationRunLedger,timed_out:bool,detail:Option<String>,gate:impl FnOnce(&mut dyn FnMut()->Result<Option<ReservationRunResult>,String>)->Result<Option<ReservationRunResult>,String>)->Result<Option<ReservationRunResult>,String> {
    checkpoint_reservation_failure(context,run_dir,input,super::run_finalization_types::DurableFinalizationDecision {outcome:if timed_out{"timed_out"}else{"failed"}.into(),reason:Some(if timed_out{"deadline_exceeded"}else{"supervisor_failed"}.into()),detail,integration_sha:None},false,gate)
}
pub fn override_failed_reservation_run(context:&RunFinalizationContext<'_>,run_dir:&Path,input:&ReservationRunLedger,detail:String,gate:impl FnOnce(&mut dyn FnMut()->Result<Option<ReservationRunResult>,String>)->Result<Option<ReservationRunResult>,String>)->Result<Option<ReservationRunResult>,String> {
    checkpoint_reservation_failure(context,run_dir,input,super::run_finalization_types::DurableFinalizationDecision {outcome:"failed".into(),reason:Some("spawn_failed".into()),detail:Some(detail),integration_sha:None},true,gate)
}
pub fn abandon_reservation_run(context:&RunFinalizationContext<'_>,run_dir:&Path,input:&ReservationRunLedger,mut classify:impl FnMut(Option<u64>,Option<Option<&str>>)->super::run_liveness::RunProcessVerdict,gate:impl FnOnce(&mut dyn FnMut()->Result<Option<ReservationRunResult>,String>)->Result<Option<ReservationRunResult>,String>)->Result<Option<ReservationRunResult>,String> {
    let claimed=with_run_finalization_claim(&context.identity.paths.locks,run_dir,input.run_id(),||gate(&mut || {
        let active=context.reservation.read_state()?.active;
        let precedence=super::run_terminal_precedence::check_run_abandonment_precedence(run_dir,input.run_id(),&mut classify,|path,id,attempt,kind|super::run_terminal_claim::claim_run_terminal(path,id,attempt,kind,std::process::id())).map_err(|error|format!("{error:?}"))?;
        match precedence.decision {
            super::run_terminal_precedence::AbandonmentDecision::Veto=>Ok(None),
            super::run_terminal_precedence::AbandonmentDecision::Finalize=> {
                let child:RunOutcome=read_run_json(&run_dir.join("outcome.json")).map_err(|error|error.to_string())?;
                let decision=resolve_finalization_decision(context.identity,run_dir,&precedence.ledger,&child)?;
                settle_reservation_run(context.identity,context.reservation,run_dir,&precedence.ledger,decision,(context.now_ms)(),context.launch).map(Some)
            },
            super::run_terminal_precedence::AbandonmentDecision::Abandon=> {
                let abandoned=chrono::DateTime::from_timestamp_millis((context.now_ms)()).ok_or("Invalid abandonment timestamp")?.to_rfc3339_opts(chrono::SecondsFormat::Millis,true);
                super::run_artifacts::write_run_json_atomic(&run_dir.join("abandoned.json"),&serde_json::json!({"version":1,"runId":precedence.ledger.run_id(),"outcome":"abandoned_unknown","abandonedAt":abandoned}),0o600).map_err(|error|error.to_string())?;
                if active.as_ref().is_some_and(|run|run.run_id==precedence.ledger.run_id()) {
                    let transition=context.reservation.complete(precedence.ledger.run_id(),memory_core::reflection::ReflectionOutcome::Failed)?;
                    if let (Some(run),Some(launch))=(&transition.launch,context.launch){launch(run);}
                }
                Ok(Some(ReservationRunResult{run_id:precedence.ledger.run_id().into(),outcome:"abandoned_unknown".into(),reason:None,detail:None,completion:None,launch:None}))
            },
        }
    }))?;
    Ok(match claimed {ClaimedRunResult::Completed(result)=>result,ClaimedRunResult::Terminal(result)=>Some(ReservationRunResult{run_id:result.run_id,outcome:result.outcome,reason:None,detail:None,completion:None,launch:None}),ClaimedRunResult::Busy=>None})
}
fn checkpoint_reservation_failure(context:&RunFinalizationContext<'_>,run_dir:&Path,input:&ReservationRunLedger,decision:super::run_finalization_types::DurableFinalizationDecision,override_outcome:bool,gate:impl FnOnce(&mut dyn FnMut()->Result<Option<ReservationRunResult>,String>)->Result<Option<ReservationRunResult>,String>)->Result<Option<ReservationRunResult>,String> {
    let claimed=with_run_finalization_claim(&context.identity.paths.locks,run_dir,input.run_id(),||gate(&mut || {
        let ledger=parse_reservation_run_ledger(read_run_json(&run_dir.join("ledger.json")).map_err(|error|error.to_string())?).map_err(|error|error.to_string())?;
        if ledger.run_id()!=input.run_id(){return Err(format!("Finalization ledger mismatch: {}",input.run_id()));}
        let attempt=ledger.value()["attempt"].as_u64().map(|attempt|u32::try_from(attempt).map_err(|error|error.to_string())).transpose()?;
        if !override_outcome&&run_dir.join("outcome.json").exists() {
            let child:RunOutcome=read_run_json(&run_dir.join("outcome.json")).map_err(|error|error.to_string())?;
            if run_outcome_matches_ledger(attempt,&child) {
                let decision=resolve_finalization_decision(context.identity,run_dir,&ledger,&child)?;
                return settle_reservation_run(context.identity,context.reservation,run_dir,&ledger,decision,(context.now_ms)(),context.launch).map(Some);
            }
        }
        let mut fields=serde_json::Map::from_iter([("finalizeOutcome".into(),decision.outcome.clone().into())]);
        if let Some(reason)=&decision.reason{fields.insert("finalizeReason".into(),reason.clone().into());}
        if let Some(detail)=&decision.detail{fields.insert("finalizeDetail".into(),detail.clone().into());}
        super::run_artifacts::update_run_ledger(&run_dir.join("ledger.json"),&fields).map_err(|error|error.to_string())?;
        let worktree=super::reservation_run_ledger::worktree_from_ledger(&context.identity.paths.repo,&context.identity.id,&ledger).map_err(|error|error.to_string())?;
        let cleanup=memory_core::reflection::cleanup_reflection_worktree(&worktree);
        if !cleanup.worktree_removed||!cleanup.branch_removed{return Err(format!("Reflection cleanup incomplete for {}",ledger.run_id()));}
        let result=super::run_finalization_types::DurableFinalizationDecision{outcome:decision.outcome.clone(),reason:decision.reason.clone(),detail:decision.detail.clone(),integration_sha:None};
        settle_reservation_run(context.identity,context.reservation,run_dir,&ledger,result,(context.now_ms)(),context.launch).map(Some)
    }))?;
    Ok(match claimed {ClaimedRunResult::Completed(result)=>result,ClaimedRunResult::Terminal(result)=>Some(ReservationRunResult{run_id:result.run_id,outcome:result.outcome,reason:None,detail:None,completion:None,launch:None}),ClaimedRunResult::Busy=>None})
}

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
    struct Inactive;
    impl super::super::runner_types::ReflectionReservationPort for Inactive {
        fn read_state(&self)->Result<memory_core::reflection::ReservationState,String>{Ok(Default::default())}
        fn complete(&self,_:&str,_:memory_core::reflection::ReflectionOutcome)->Result<memory_core::reflection::CompletionResult,String>{panic!("inactive reservation")}
    }
    #[test]
    fn failure_checkpoints_and_cleans_before_terminal_settlement() {
        for timed_out in [false,true] {
            let root=tempfile::tempdir().unwrap();
            let identity=memory_core::identity::resolve::MemoryIdentity {id:"agent".into(),safe_slug:"agent".into(),paths:memory_core::identity::layout::build_identity_paths(root.path(),"agent")};
            let engine=crate::engine_session::prepare_memory_engine_session("agent",&identity.paths,Default::default()).unwrap();
            let exec=memory_core::git::exec::create_git_exec(Default::default());
            let worktree=memory_core::reflection::create_reflection_worktree(&engine.repo,"run",&identity.paths.worktrees,exec.as_ref(),None).unwrap();
            let dir=root.path().join("run");std::fs::create_dir_all(&dir).unwrap();
            let value=serde_json::json!({"version":1,"runId":"run","kind":"reflection","trigger":"manual","category":"quick","conversationIds":["conversation"],"startedAt":"2026-08-16T00:00:00.000Z","hardDeadlineAt":100,"terminationGraceMs":5,"deadlineAt":105,"mergePolicy":"auto","worktreeDir":worktree.dir,"worktreeBranch":worktree.branch,"baseSha":worktree.base_commit_sha,"gitFilePath":worktree.git_file_path,"gitFileSnapshot":worktree.git_file_snapshot,"commonConfigPath":worktree.common_config_path,"commonConfigSnapshot":worktree.common_config_snapshot});
            super::super::run_artifacts::write_run_json_atomic(&dir.join("ledger.json"),&value,0o600).unwrap();
            let ledger=parse_reservation_run_ledger(value).unwrap();
            let context=RunFinalizationContext {identity:&identity,reservation:&Inactive,launch:None,now_ms:&||1786838400000};
            let result=fail_reservation_run(&context,&dir,&ledger,timed_out,Some("supervisor detail".into()),|operation|operation()).unwrap().unwrap();
            assert_eq!(result.outcome,if timed_out{"timed_out"}else{"failed"});assert_eq!(result.reason.as_deref(),Some(if timed_out{"deadline_exceeded"}else{"supervisor_failed"}));
            assert!(!worktree.dir.exists());assert!(dir.join("final.json").exists());assert!(result.completion.is_some());
            let checkpoint:serde_json::Value=read_run_json(&dir.join("ledger.json")).unwrap();assert_eq!(checkpoint["finalizePhase"],"settled");
        }
    }
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
