use std::path::Path;
use memory_core::facts::{FactsFailureReason,FactsFailureTarget,FactsQueueEntry,failures_store::{FactsFailureStoreError,RecordFailureRequest}};
use crate::{facts_failure_recording::{FactsFailurePort,ledger_targets},facts_runner_types::{FactsRunLedger,FactsTerminalOutcome},worker::run_artifacts::ArtifactError};
#[derive(Debug)]
pub enum FactsTerminalError{Failure(FactsFailureStoreError),Artifact(ArtifactError),Consume(std::io::Error)}
impl std::fmt::Display for FactsTerminalError{fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result{match self{Self::Failure(error)=>error.fmt(f),Self::Artifact(error)=>error.fmt(f),Self::Consume(error)=>error.fmt(f)}}}
impl std::error::Error for FactsTerminalError{}
pub trait FactsTerminalIo{
    fn now(&self)->String;
    fn mark_consumed(&mut self,entries:&[FactsQueueEntry])->std::io::Result<()>;
    fn write_sentinel(&mut self,path:&Path,value:&serde_json::Value)->Result<(),ArtifactError>{crate::worker::run_artifacts::write_run_json_atomic(path,value,0o600)}
    fn remove(&mut self,path:&Path)->std::io::Result<()>{crate::facts_run_cleanup::remove_run_artifact(path)}
    fn warn(&mut self,message:&str,detail:&str);
}
pub struct FactsFailureWrite<'a>{pub run_dir:&'a Path,pub run_id:&'a str,pub batch_id:&'a str,pub targets:&'a [FactsFailureTarget],pub reason:FactsFailureReason,pub detail:&'a str,pub outcome:Option<FactsTerminalOutcome>}
pub struct FactsTerminalWrites<'a>{pub failures:&'a dyn FactsFailurePort,pub io:&'a mut dyn FactsTerminalIo}
impl FactsTerminalWrites<'_>{
    fn record(&self,targets:&[FactsFailureTarget],id:&str,reason:FactsFailureReason,detail:&str)->Result<(),FactsTerminalError>{if targets.is_empty(){return Ok(());}self.failures.record_failure(RecordFailureRequest{targets:targets.to_vec(),failure_id:id.into(),reason,detail:Some(detail.into())}).map_err(FactsTerminalError::Failure)?;Ok(())}
    fn cleanup(&mut self,dir:&Path){for name in ["facts-payload.json",".sandbox-tmp"]{let path=dir.join(name);if let Err(error)=self.io.remove(&path)&&error.kind()!=std::io::ErrorKind::NotFound{self.io.warn("facts run artifact cleanup failed",&format!("{}: {error}",path.display()));}}}
    fn final_record(&mut self,dir:&Path,id:&str,outcome:FactsTerminalOutcome,detail:Option<&str>,sha:Option<&str>)->Result<(),FactsTerminalError>{
        let mut record=serde_json::json!({"version":1,"runId":id,"outcome":outcome,"finishedAt":self.io.now()});
        if let Some(detail)=detail{record["detail"]=detail.into();}
        if let Some(sha)=sha{record["sha"]=sha.into();}
        self.io.write_sentinel(&dir.join("final.json"),&record).map_err(FactsTerminalError::Artifact)?;
        self.cleanup(dir);
        Ok(())
    }
    pub fn fail(&mut self,write:&FactsFailureWrite<'_>)->Result<(),FactsTerminalError>{self.record(write.targets,write.batch_id,write.reason,write.detail)?;self.final_record(write.run_dir,write.run_id,write.outcome.unwrap_or(FactsTerminalOutcome::Failed),Some(write.detail),None)}
    pub fn abandon(&mut self,dir:&Path,ledger:&FactsRunLedger)->Result<(),FactsTerminalError>{self.record(&ledger_targets(&ledger.queued),&ledger.batch_id,FactsFailureReason::UnknownLiveness,"facts run liveness is unknown")?;self.io.write_sentinel(&dir.join("abandoned.json"),&serde_json::json!({"version":1,"runId":ledger.run_id,"abandonedAt":self.io.now(),"reason":"unknown_liveness"})).map_err(FactsTerminalError::Artifact)?;self.cleanup(dir);Ok(())}
    pub fn preflight_fail(&self,targets:&[FactsFailureTarget],id:&str,reason:FactsFailureReason,detail:&str)->Result<(),FactsTerminalError>{self.record(targets,id,reason,detail)}
    pub fn succeed(&mut self,dir:&Path,id:&str,outcome:FactsTerminalOutcome,entries:&[FactsQueueEntry],targets:&[FactsFailureTarget],sha:Option<&str>)->Result<(),FactsTerminalError>{self.io.mark_consumed(entries).map_err(FactsTerminalError::Consume)?;if !targets.is_empty()&&let Err(error)=self.failures.clear_on_success(targets){self.io.warn("facts failure records survived a successful run",&error.to_string());}self.final_record(dir,id,outcome,None,sha)}
}
impl crate::facts_oversize::FactsPreflightFailure for FactsTerminalWrites<'_>{type Error=FactsTerminalError;fn preflight_fail(&mut self,targets:Vec<FactsFailureTarget>,id:String,reason:FactsFailureReason,detail:String)->Result<(),Self::Error>{FactsTerminalWrites::preflight_fail(self,&targets,&id,reason,&detail)}}
#[cfg(test)]
mod tests{
    use super::*;
    use std::{cell::RefCell,rc::Rc};
    type Events=Rc<RefCell<Vec<&'static str>>>;
    struct Failures{events:Events,fail_record:bool,fail_clear:bool}
    impl FactsFailurePort for Failures{fn record_failure(&self,request:RecordFailureRequest)->Result<memory_core::facts::FactsFailuresFile,FactsFailureStoreError>{assert_eq!(request.failure_id,"batch");self.events.borrow_mut().push("record");if self.fail_record{return Err(FactsFailureStoreError::Io(std::io::ErrorKind::PermissionDenied.into()));}Ok(memory_core::facts::empty_failures_file("now".into()))}fn clear_on_success(&self,_:&[FactsFailureTarget])->Result<memory_core::facts::FactsFailuresFile,FactsFailureStoreError>{self.events.borrow_mut().push("clear");if self.fail_clear{return Err(FactsFailureStoreError::Io(std::io::ErrorKind::PermissionDenied.into()));}Ok(memory_core::facts::empty_failures_file("now".into()))}}
    struct Io{events:Events,fail_write:bool}
    impl FactsTerminalIo for Io{fn now(&self)->String{"2026-08-10T00:00:00Z".into()}fn mark_consumed(&mut self,_:&[FactsQueueEntry])->std::io::Result<()>{self.events.borrow_mut().push("consume");Ok(())}fn write_sentinel(&mut self,path:&Path,value:&serde_json::Value)->Result<(),ArtifactError>{self.events.borrow_mut().push("sentinel");if self.fail_write{return Err(ArtifactError::Io(std::io::ErrorKind::PermissionDenied.into()));}crate::worker::run_artifacts::write_run_json_atomic(path,value,0o600)}fn remove(&mut self,path:&Path)->std::io::Result<()>{assert!(crate::facts_run_cleanup::is_terminal_run_dir(path.parent().unwrap()));self.events.borrow_mut().push("remove");crate::facts_run_cleanup::remove_run_artifact(path)}fn warn(&mut self,_:&str,_:&str){self.events.borrow_mut().push("warn");}}
    fn target()->FactsFailureTarget{FactsFailureTarget{conversation_id:"session".into(),end_message_id:"m1".into(),end_snapshot_line:1}}
    #[test]fn failure_store_precedes_sentinel_and_deletion(){let dir=tempfile::tempdir().unwrap();std::fs::write(dir.path().join("facts-payload.json"),"payload").unwrap();let events=Events::default();let failures=Failures{events:events.clone(),fail_record:false,fail_clear:false};let mut io=Io{events:events.clone(),fail_write:false};FactsTerminalWrites{failures:&failures,io:&mut io}.fail(&FactsFailureWrite{run_dir:dir.path(),run_id:"run",batch_id:"batch",targets:&[target()],reason:FactsFailureReason::ChildExit,detail:"exit",outcome:None}).unwrap();assert_eq!(*events.borrow(),["record","sentinel","remove","remove"]);assert!(!dir.path().join("facts-payload.json").exists());assert!(dir.path().join("final.json").exists());}
    #[test]fn record_or_sentinel_failure_retains_payload(){for fail_record in [true,false]{let dir=tempfile::tempdir().unwrap();let payload=dir.path().join("facts-payload.json");std::fs::write(&payload,"payload").unwrap();let events=Events::default();let failures=Failures{events:events.clone(),fail_record,fail_clear:false};let mut io=Io{events:events.clone(),fail_write:!fail_record};assert!(FactsTerminalWrites{failures:&failures,io:&mut io}.fail(&FactsFailureWrite{run_dir:dir.path(),run_id:"run",batch_id:"batch",targets:&[target()],reason:FactsFailureReason::ChildExit,detail:"exit",outcome:None}).is_err());assert!(payload.exists());assert!(!dir.path().join("final.json").exists());assert!(!events.borrow().contains(&"remove"));}}
    #[test]fn success_consumes_before_clear_and_clear_error_does_not_veto(){let dir=tempfile::tempdir().unwrap();let events=Events::default();let failures=Failures{events:events.clone(),fail_record:false,fail_clear:true};let mut io=Io{events:events.clone(),fail_write:false};FactsTerminalWrites{failures:&failures,io:&mut io}.succeed(dir.path(),"run",FactsTerminalOutcome::Committed,&[],&[target()],Some("sha")).unwrap();assert_eq!(*events.borrow(),["consume","clear","warn","sentinel","remove","remove"]);assert!(dir.path().join("final.json").exists());}
}
