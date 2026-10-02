use std::path::Path;
use memory_core::{facts::{extraction::{FactsBatch,FactsExtractionError,FactsExtractionRecord,validate_facts_recovery},mutation_plan::{MutationPlanError,plan_facts_mutation},person_routing::FactsPeopleRouting,recovery::{FactsRecoveryResult,apply_facts_recovery}},git::{GitMemoryRepo,GitCommitAuthor,errors::GitError}};
use crate::{facts_runner_types::FactsRunLedger,worker::run_artifacts::{ArtifactError,update_run_ledger}};
#[derive(Debug)]pub enum FactsApplyError{Git(GitError),Plan(MutationPlanError),Extraction(FactsExtractionError),Recovery(memory_core::facts::recovery::FactsRecoveryError),Artifact(ArtifactError)}
#[derive(Debug,PartialEq,Eq)]pub enum Applied{Committed{sha:String},ParentDirty{detail:Option<String>}}
pub fn resolve_facts_people_routing(config:&serde_json::Value,identity:&str)->FactsPeopleRouting{
    let memory=&config["memory"];let overrides=&memory["agents"][identity]["people"];let people=&memory["people"];
    FactsPeopleRouting{
        enabled:overrides["enabled"].as_bool().or_else(||people["enabled"].as_bool()).unwrap_or(true),
        max_entries:overrides["max_entries"].as_u64().or_else(||people["max_entries"].as_u64()).unwrap_or(40) as usize,
        max_entry_chars:overrides["max_entry_chars"].as_u64().or_else(||people["max_entry_chars"].as_u64()).unwrap_or(200) as usize,
    }
}
pub fn apply_facts_with_retries<T,E>(mut operation:impl FnMut(usize)->Result<T,memory_core::locks::WithLockError<E>>,mut retry_delay:impl FnMut(usize,u64),mut random:impl FnMut()->f64)->Result<Option<T>,memory_core::locks::WithLockError<E>>{
    for attempt in 1..=3{
        match operation(attempt){
            Ok(applied)=>return Ok(Some(applied)),
            Err(memory_core::locks::WithLockError::Acquire(memory_core::locks::AcquireLockError::Contention(_)))=>{
                if attempt==3{return Ok(None);}
                retry_delay(attempt,25+(random()*76.0).floor() as u64);
            },
            Err(error)=>return Err(error),
        }
    }
    Ok(None)
}
pub fn apply_claimed(run_dir:&Path,ledger:&FactsRunLedger,repo:&GitMemoryRepo,records:&[FactsExtractionRecord],people:&FactsPeopleRouting,identity:&str)->Result<Applied,FactsApplyError>{
    if let Some(receipt)=repo.log(None).map_err(FactsApplyError::Git)?.into_iter().find(|entry|entry.trailers.get("Omo-Facts-Batch")==Some(&ledger.batch_id)){return Ok(Applied::Committed{sha:receipt.sha});}
    let batch=FactsBatch{batch_id:ledger.batch_id.clone(),records:records.to_vec()};
    let recovery=if let Some(recovery)=&ledger.apply_recovery{validate_facts_recovery(recovery,&batch).map_err(FactsApplyError::Extraction)?;recovery.clone()}else{
        let recovery=match plan_facts_mutation(repo,&batch,Some(people),None){Ok(recovery)=>recovery,Err(MutationPlanError::ParentDirty(_))=>return Ok(Applied::ParentDirty{detail:None}),Err(error)=>return Err(FactsApplyError::Plan(error))};
        let fields=serde_json::Map::from_iter([("headBeforeApply".into(),serde_json::Value::String(recovery.head_before_apply.clone())),("applyRecovery".into(),serde_json::to_value(&recovery).map_err(|error|FactsApplyError::Artifact(ArtifactError::Json(error)))?)]);update_run_ledger(&run_dir.join("ledger.json"),&fields).map_err(FactsApplyError::Artifact)?;recovery
    };
    match apply_facts_recovery(repo,&recovery,records.len(),&GitCommitAuthor{agent_id:identity.into(),author_name:"Facts Extractor".into(),author_email:None}).map_err(FactsApplyError::Recovery)?{FactsRecoveryResult::Committed{sha,..}=>Ok(Applied::Committed{sha}),FactsRecoveryResult::ParentDirty{detail}=>Ok(Applied::ParentDirty{detail})}
}
#[cfg(test)]mod tests{
    use super::*;
    #[test]fn people_routing_resolves_agent_then_global_then_defaults(){
        let config=serde_json::json!({"memory":{"people":{"enabled":true,"max_entries":17},"agents":{"agent":{"people":{"enabled":false,"max_entry_chars":91}}}}});
        assert_eq!(resolve_facts_people_routing(&config,"agent"),FactsPeopleRouting{enabled:false,max_entries:17,max_entry_chars:91});
        assert_eq!(resolve_facts_people_routing(&config,"other"),FactsPeopleRouting{enabled:true,max_entries:17,max_entry_chars:200});
        assert_eq!(resolve_facts_people_routing(&serde_json::json!({}),"agent"),FactsPeopleRouting{enabled:true,max_entries:40,max_entry_chars:200});
    }
    fn contention()->memory_core::locks::WithLockError<String>{memory_core::locks::WithLockError::Acquire(memory_core::locks::AcquireLockError::Contention(Box::new(memory_core::locks::LockContentionError::new("writer.lock".into(),None))))}
    #[test]fn contention_retries_with_injected_jitter_before_success(){
        let mut attempts=vec![];let mut delays=vec![];
        let result=apply_facts_with_retries(|attempt|{attempts.push(attempt);if attempt<3{Err(contention())}else{Ok("committed")}},|attempt,delay|delays.push((attempt,delay)),||0.5).unwrap();
        assert_eq!(result,Some("committed"));assert_eq!(attempts,[1,2,3]);assert_eq!(delays,[(1,63),(2,63)]);
    }
    #[test]fn contention_exhaustion_stops_after_three_attempts(){
        let mut attempts=0;let mut delays=0;
        let result=apply_facts_with_retries::<(),String>(|_|{attempts+=1;Err(contention())},|_,_|delays+=1,||0.0).unwrap();
        assert_eq!(result,None);assert_eq!(attempts,3);assert_eq!(delays,2);
    }
    #[test]fn noncontention_error_never_retries(){
        let error=apply_facts_with_retries::<(),String>(|_|Err(memory_core::locks::WithLockError::Acquire(memory_core::locks::AcquireLockError::Aborted)),|_,_|panic!("must not delay"),||panic!("must not sample jitter")).unwrap_err();
        assert!(matches!(error,memory_core::locks::WithLockError::Acquire(memory_core::locks::AcquireLockError::Aborted)));
    }
    fn ledger()->FactsRunLedger{serde_json::from_value(serde_json::json!({"version":1,"runId":"run","kind":"facts","startedAt":"now","hardDeadlineAt":1,"terminationGraceMs":1,"deadlineAt":2,"batchId":"c04edbea-90ea-4c67-9c22-1a9beb207535","queued":[]})).unwrap()}
    #[test]fn durable_recovery_precedes_commit_and_receipt_makes_replay_idempotent(){let root=tempfile::tempdir().unwrap();let paths=memory_core::identity::layout::build_identity_paths(root.path(),"agent");let engine=crate::engine_session::prepare_memory_engine_session("agent",&paths,Default::default()).unwrap();let run=root.path().join("run");std::fs::create_dir(&run).unwrap();let ledger=ledger();crate::worker::run_artifacts::write_run_json_atomic(&run.join("ledger.json"),&ledger,0o600).unwrap();let records=vec![FactsExtractionRecord::Project{text:"Project uses a durable journal".into(),date:"2026-08-12".into()}];let people=FactsPeopleRouting{enabled:true,max_entries:40,max_entry_chars:200};let first=apply_claimed(&run,&ledger,&engine.repo,&records,&people,"agent").unwrap();let persisted:FactsRunLedger=crate::worker::run_artifacts::read_run_json(&run.join("ledger.json")).unwrap();assert!(persisted.apply_recovery.is_some());assert!(persisted.head_before_apply.is_some());let sha=engine.repo.head().unwrap();let second=apply_claimed(&run,&persisted,&engine.repo,&records,&people,"agent").unwrap();assert_eq!(first,second);assert_eq!(engine.repo.head().unwrap(),sha);assert!(engine.repo.status(&[] as &[&str]).unwrap().trim().is_empty());}
    #[test]fn foreign_dirty_worktree_is_retained(){let root=tempfile::tempdir().unwrap();let paths=memory_core::identity::layout::build_identity_paths(root.path(),"agent");let engine=crate::engine_session::prepare_memory_engine_session("agent",&paths,Default::default()).unwrap();let foreign=paths.repo.join("foreign.txt");std::fs::write(&foreign,"user change").unwrap();let records=vec![FactsExtractionRecord::Project{text:"fact".into(),date:"2026-08-12".into()}];let people=FactsPeopleRouting{enabled:true,max_entries:40,max_entry_chars:200};assert_eq!(apply_claimed(root.path(),&ledger(),&engine.repo,&records,&people,"agent").unwrap(),Applied::ParentDirty{detail:None});assert_eq!(std::fs::read_to_string(foreign).unwrap(),"user change");assert!(!root.path().join("ledger.json").exists());}
}
