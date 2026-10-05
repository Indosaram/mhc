use std::path::Path;
use memory_core::locks::ProcessLiveness;
use crate::{facts_runner_types::FactsRunLedger,worker::run_artifacts::{ArtifactError,RunOutcome,read_run_json,run_outcome_matches_ledger}};
pub trait FactsReconciliationPort{
    type Error;
    fn now_ms(&self)->f64;
    fn liveness(&self,ledger:&FactsRunLedger)->ProcessLiveness{crate::facts_run_storage::run_liveness(ledger)}
    fn finalize(&mut self,dir:&Path)->Result<(),Self::Error>;
    fn fail(&mut self,dir:&Path,ledger:&FactsRunLedger,detail:&str)->Result<(),Self::Error>;
    fn abandon(&mut self,dir:&Path,ledger:&FactsRunLedger)->Result<(),Self::Error>;
    fn warn(&mut self,run_id:&str,error:&Self::Error);
}
#[derive(Debug)]
pub enum FactsReconciliationError<E>{Artifact(ArtifactError),Operation(E)}
pub fn reconcile_facts_runs<P:FactsReconciliationPort>(facts:&Path,port:&mut P)->Result<bool,FactsReconciliationError<P::Error>>{
    let runs=facts.join("runs");let mut names:Vec<_>=std::fs::read_dir(&runs).into_iter().flatten().filter_map(Result::ok).map(|entry|entry.file_name()).collect();names.sort();let mut active=false;
    for name in names{
        let dir=runs.join(name);
        if crate::facts_run_cleanup::is_terminal_run_dir(&dir){continue;}
        let Ok(ledger)=read_run_json::<FactsRunLedger>(&dir.join("ledger.json"))else{continue;};
        if dir.join("outcome.json").exists(){let outcome=read_run_json::<RunOutcome>(&dir.join("outcome.json")).map_err(FactsReconciliationError::Artifact)?;if run_outcome_matches_ledger(ledger.attempt,&outcome){if let Err(error)=port.finalize(&dir){port.warn(&ledger.run_id,&error);active=true;}continue;}}
        let verdict=port.liveness(&ledger);
        if verdict==ProcessLiveness::Alive||port.now_ms()<=ledger.deadline_at{active=true;continue;}
        if verdict==ProcessLiveness::Unknown{port.abandon(&dir,&ledger).map_err(FactsReconciliationError::Operation)?;}else{port.fail(&dir,&ledger,"facts supervisor and child are not alive").map_err(FactsReconciliationError::Operation)?;}
    }
    Ok(active)
}
#[cfg(test)]
mod tests{
    use super::*;
    struct Port{events:Vec<String>,verdict:ProcessLiveness,now:f64,finalize_fails:bool}
    impl FactsReconciliationPort for Port{type Error=&'static str;fn now_ms(&self)->f64{self.now}fn liveness(&self,_:&FactsRunLedger)->ProcessLiveness{self.verdict}fn finalize(&mut self,_:&Path)->Result<(),Self::Error>{self.events.push("finalize".into());if self.finalize_fails{Err("busy")}else{Ok(())}}fn fail(&mut self,_:&Path,ledger:&FactsRunLedger,_:&str)->Result<(),Self::Error>{self.events.push(format!("fail:{}",ledger.run_id));Ok(())}fn abandon(&mut self,_:&Path,ledger:&FactsRunLedger)->Result<(),Self::Error>{self.events.push(format!("abandon:{}",ledger.run_id));Ok(())}fn warn(&mut self,_:&str,_:&Self::Error){self.events.push("warn".into());}}
    fn seed(facts:&Path)->std::path::PathBuf{let dir=facts.join("runs/run");std::fs::create_dir_all(&dir).unwrap();crate::worker::run_artifacts::write_run_json_atomic(&dir.join("ledger.json"),&serde_json::json!({"version":1,"runId":"run","kind":"facts","startedAt":"now","hardDeadlineAt":95,"terminationGraceMs":5,"deadlineAt":100,"batchId":"batch","queued":[]}),0o600).unwrap();dir}
    #[test]fn matching_outcome_finalizes_and_error_remains_active(){let root=tempfile::tempdir().unwrap();let dir=seed(root.path());crate::worker::run_artifacts::write_run_json_atomic(&dir.join("outcome.json"),&RunOutcome{version:1,run_id:"run".into(),attempt:None,finished_at:"now".into(),child_exit:crate::worker::run_artifacts::ChildExit{code:Some(0),signal:None},timed_out:false},0o600).unwrap();for fail in [false,true]{let mut port=Port{events:vec![],verdict:ProcessLiveness::Unknown,now:200.0,finalize_fails:fail};assert_eq!(reconcile_facts_runs(root.path(),&mut port).unwrap(),fail);assert_eq!(port.events,if fail{vec!["finalize","warn"]}else{vec!["finalize"]});}}
    #[test]fn expired_dead_fails_unknown_abandons_alive_keeps(){let root=tempfile::tempdir().unwrap();seed(root.path());for (verdict,active,event) in [(ProcessLiveness::Dead,false,Some("fail:run")),(ProcessLiveness::Unknown,false,Some("abandon:run")),(ProcessLiveness::Alive,true,None)]{let mut port=Port{events:vec![],verdict,now:101.0,finalize_fails:false};assert_eq!(reconcile_facts_runs(root.path(),&mut port).unwrap(),active);assert_eq!(port.events,event.into_iter().map(str::to_owned).collect::<Vec<_>>());}}
    #[test]fn deadline_boundary_and_terminal_skip(){let root=tempfile::tempdir().unwrap();let dir=seed(root.path());let mut port=Port{events:vec![],verdict:ProcessLiveness::Dead,now:100.0,finalize_fails:false};assert!(reconcile_facts_runs(root.path(),&mut port).unwrap());assert!(port.events.is_empty());std::fs::write(dir.join("abandoned.json"),"{}").unwrap();assert!(!reconcile_facts_runs(root.path(),&mut port).unwrap());}
}
