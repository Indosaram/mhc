use std::path::Path;
use serde_json::Value;
use super::{reservation_run_ledger::{ReservationRunLedger,ReservationLedgerError,parse_reservation_run_ledger},run_artifacts::{ArtifactError,read_run_json},run_liveness::RunProcessVerdict,run_terminal_claim::{RunTerminalClaim,RunTerminalClaimKind,TerminalClaimError,run_terminal_claim_matches}};
#[derive(Debug,PartialEq,Eq)]pub enum AbandonmentDecision{Finalize,Veto,Abandon}
pub struct RunAbandonmentPrecedence{pub decision:AbandonmentDecision,pub ledger:ReservationRunLedger}
#[derive(Debug)]pub enum PrecedenceError{Artifact(ArtifactError),Ledger(ReservationLedgerError),Mismatch(String),Claim(TerminalClaimError)}
fn read_terminal_publication(run_dir:&Path,ledger:&ReservationRunLedger)->Result<Option<AbandonmentDecision>,PrecedenceError>{
    let outcome=run_dir.join("outcome.json");
    if outcome.exists(){let outcome:Value=read_run_json(&outcome).map_err(PrecedenceError::Artifact)?;if outcome.get("attempt")==ledger.value().get("attempt"){return Ok(Some(AbandonmentDecision::Finalize));}}
    let publishing=run_dir.join("publishing.json");
    if !publishing.exists(){return Ok(None);}
    let publishing:Value=read_run_json(&publishing).map_err(PrecedenceError::Artifact)?;
    Ok((publishing["version"]==1&&publishing["runId"]==ledger.run_id()&&publishing.get("attempt")==ledger.value().get("attempt")).then_some(AbandonmentDecision::Veto))
}
pub fn check_run_abandonment_precedence(
    run_dir:&Path,run_id:&str,
    mut classify:impl FnMut(Option<u64>,Option<Option<&str>>)->RunProcessVerdict,
    mut claim:impl FnMut(&Path,&str,i64,RunTerminalClaimKind)->Result<RunTerminalClaim,TerminalClaimError>,
)->Result<RunAbandonmentPrecedence,PrecedenceError>{
    let ledger=parse_reservation_run_ledger(read_run_json(&run_dir.join("ledger.json")).map_err(PrecedenceError::Artifact)?).map_err(PrecedenceError::Ledger)?;
    if ledger.run_id()!=run_id{return Err(PrecedenceError::Mismatch(format!("Finalization ledger mismatch: {run_id}")));}
    if let Some(decision)=read_terminal_publication(run_dir,&ledger)?{return Ok(RunAbandonmentPrecedence{decision,ledger});}
    let value=ledger.value();
    let supervisor=classify(value["pid"].as_u64(),value.get("processStart").map(Value::as_str));
    let child=classify(value["childPid"].as_u64(),value.get("childProcessStart").map(Value::as_str));
    if supervisor==RunProcessVerdict::Alive||child==RunProcessVerdict::Alive{return Ok(RunAbandonmentPrecedence{decision:AbandonmentDecision::Veto,ledger});}
    if let Some(decision)=read_terminal_publication(run_dir,&ledger)?{return Ok(RunAbandonmentPrecedence{decision,ledger});}
    let attempt=value["attempt"].as_i64().unwrap_or(1);
    let terminal=claim(run_dir,ledger.run_id(),attempt,RunTerminalClaimKind::Abandon).map_err(PrecedenceError::Claim)?;
    let decision=if run_terminal_claim_matches(&terminal,ledger.run_id(),attempt,Some(RunTerminalClaimKind::Abandon)){AbandonmentDecision::Abandon}else{AbandonmentDecision::Veto};
    Ok(RunAbandonmentPrecedence{decision,ledger})
}
#[cfg(test)]mod tests{
    use super::*;
    use super::super::{run_artifacts::write_run_json_atomic,run_terminal_claim::claim_run_terminal};
    fn fixture()->tempfile::TempDir{let dir=tempfile::tempdir().unwrap();write_run_json_atomic(&dir.path().join("ledger.json"),&serde_json::json!({"version":1,"runId":"run","attempt":1,"kind":"reflection","trigger":"manual","startedAt":"now","hardDeadlineAt":1,"terminationGraceMs":1,"deadlineAt":2,"mergePolicy":"auto","worktreeDir":"missing","worktreeBranch":"memory/run","baseSha":"sha","gitFilePath":"missing/.git","gitFileSnapshot":"gitdir: missing","commonConfigPath":"config","commonConfigSnapshot":null,"pid":424242,"processStart":null}),0o600).unwrap();dir}
    fn claim(path:&Path,id:&str,attempt:i64,kind:RunTerminalClaimKind)->Result<RunTerminalClaim,TerminalClaimError>{claim_run_terminal(path,id,attempt,kind,std::process::id())}
    #[test]fn publication_during_probe_vetoes_abandonment(){let dir=fixture();let result=check_run_abandonment_precedence(dir.path(),"run",|_,_|{write_run_json_atomic(&dir.path().join("publishing.json"),&serde_json::json!({"version":1,"runId":"run","attempt":1}),0o600).unwrap();RunProcessVerdict::Unknown},claim).unwrap();assert_eq!(result.decision,AbandonmentDecision::Veto);assert!(!dir.path().join("terminal-claim.json").exists());}
    #[test]fn unknown_liveness_without_publication_abandons(){let dir=fixture();assert_eq!(check_run_abandonment_precedence(dir.path(),"run",|_,_|RunProcessVerdict::Unknown,claim).unwrap().decision,AbandonmentDecision::Abandon);}
    #[test]fn existing_publish_claim_vetoes(){let dir=fixture();claim_run_terminal(dir.path(),"run",1,RunTerminalClaimKind::Publish,std::process::id()).unwrap();assert_eq!(check_run_abandonment_precedence(dir.path(),"run",|_,_|RunProcessVerdict::Unknown,claim).unwrap().decision,AbandonmentDecision::Veto);}
    #[test]fn matching_outcome_finalizes_before_liveness(){let dir=fixture();write_run_json_atomic(&dir.path().join("outcome.json"),&serde_json::json!({"attempt":1}),0o600).unwrap();assert_eq!(check_run_abandonment_precedence(dir.path(),"run",|_,_|panic!("probe"),claim).unwrap().decision,AbandonmentDecision::Finalize);}
    #[test]fn alive_run_vetoes_and_mismatched_identity_fails(){let dir=fixture();assert_eq!(check_run_abandonment_precedence(dir.path(),"run",|_,_|RunProcessVerdict::Alive,claim).unwrap().decision,AbandonmentDecision::Veto);assert!(matches!(check_run_abandonment_precedence(dir.path(),"other",|_,_|panic!("probe"),claim),Err(PrecedenceError::Mismatch(_))));}
}
