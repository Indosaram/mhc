use std::collections::BTreeMap;
use memory_core::git::{GitMemoryRepo,errors::GitError};
use serde::Serialize;
use crate::context::MemoryIdentityContext;
#[derive(Debug,Serialize)]
#[serde(rename_all="camelCase")]
pub struct MemoryRpcRepoState{
    #[serde(skip_serializing_if="Option::is_none")]pub head_sha:Option<String>,
    #[serde(skip_serializing_if="Option::is_none")]pub head_subject:Option<String>,
    #[serde(rename="committedAtISO",skip_serializing_if="Option::is_none")]pub committed_at_iso:Option<String>,
    pub dirty:bool,pub dirty_paths:usize,pub system_tokens_estimate:usize,
}
#[derive(Default,Debug,Serialize,PartialEq)]
#[serde(rename_all="camelCase")]
pub struct MemoryRpcJournalState{pub backlog_steps:f64,pub pending_compaction:bool,pub total_steps:f64,pub reflected_steps:f64}
fn count(value:Option<&serde_json::Value>)->f64{value.and_then(serde_json::Value::as_f64).filter(|number|number.is_finite()&&*number>0.0).map(f64::floor).unwrap_or(0.0)}
pub fn read_memory_rpc_journal_state(context:&MemoryIdentityContext,session:&str)->MemoryRpcJournalState{
    let empty=MemoryRpcJournalState{pending_compaction:context.ledger.pending_compaction,..Default::default()};
    let Some(value)=std::fs::read(context.identity_paths.transcripts.join(session).join("state.json")).ok().and_then(|bytes|serde_json::from_slice::<serde_json::Value>(&bytes).ok())else{return empty;};
    let Some(state)=value.as_object()else{return empty;};
    MemoryRpcJournalState{backlog_steps:count(state.get("steps_since_last_successful_reflection")),pending_compaction:state.get("pending_compaction")==Some(&serde_json::Value::Bool(true))||empty.pending_compaction,total_steps:count(state.get("total_completed_steps")),reflected_steps:count(state.get("reflected_completed_steps"))}
}
pub fn read_memory_rpc_repo_state(repo:&GitMemoryRepo,cache:&mut BTreeMap<String,usize>)->Result<MemoryRpcRepoState,GitError>{
    let head=repo.head()?;let dirty_paths=repo.status(&[] as &[&str]).map(|status|status.lines().filter(|line|!line.trim().is_empty()).count()).unwrap_or(0);
    let mut state=MemoryRpcRepoState{head_sha:head.clone(),head_subject:None,committed_at_iso:None,dirty:dirty_paths>0,dirty_paths,system_tokens_estimate:0};
    let Some(head)=head else{return Ok(state);};
    state.head_subject=repo.log(Some(&memory_core::git::GitLogOptions{limit:Some(1),..Default::default()})).ok().and_then(|entries|entries.first().map(|entry|entry.subject.clone()));
    state.committed_at_iso=repo.head_commit_timestamp().ok().flatten().and_then(|timestamp|chrono::DateTime::from_timestamp(timestamp,0)).map(|time|time.to_rfc3339_opts(chrono::SecondsFormat::Millis,true));
    state.system_tokens_estimate=if let Some(estimate)=cache.get(&head){*estimate}else{
        let estimate=repo.ls_tree(Some(&head),None).and_then(|paths|{let mut bytes=0;for path in paths.iter().filter(|path|path.starts_with("system/")&&path.ends_with(".md")){bytes+=repo.show(&head,path)?.len();}Ok(bytes/4)});
        match estimate{Ok(estimate)=>{cache.insert(head,estimate);estimate},Err(_)=>0}
    };
    Ok(state)
}
pub fn read_memory_rpc_health(context:&MemoryIdentityContext,now:i64)->serde_json::Value{
    let health=crate::worker::health::read_reflection_health(&context.identity_paths.reflection.join("completions"),crate::status::MEMORY_HEALTH_SCAN_LIMIT,now);
    let mut result=serde_json::json!({"streak":health.streak});
    if !health.fingerprint.is_empty(){result["fingerprint"]=health.fingerprint.into();}
    if let Some(last)=health.last_outcome{let mut value=serde_json::json!({"runId":last.run_id,"outcome":last.outcome,"finishedAt":last.finished_at});if let Some(reason)=last.reason{value["reason"]=reason.into();}result["lastOutcome"]=value;}
    result
}
#[cfg(test)]
mod tests{
    use super::*;
    fn context(root:&std::path::Path)->MemoryIdentityContext{MemoryIdentityContext::new("agent".into(),memory_core::identity::layout::build_identity_paths(root,"agent"),crate::binding::MemorySessionBinding{identity:"agent".into(),repo_path_hash:"hash".into(),bound_at:0.0})}
    #[test]fn real_repo_estimates_committed_system_only_and_cache_by_sha(){let root=tempfile::tempdir().unwrap();let context=context(root.path());let engine=crate::engine_session::prepare_memory_engine_session("agent",&context.identity_paths,Default::default()).unwrap();let mut cache=BTreeMap::new();let state=read_memory_rpc_repo_state(&engine.repo,&mut cache).unwrap();assert!(!state.dirty);assert!(state.head_subject.is_some());assert!(state.committed_at_iso.as_ref().unwrap().ends_with('Z'));assert!(state.system_tokens_estimate>0);std::fs::write(context.identity_paths.repo.join("system/persona.md"),"uncommitted change").unwrap();let next=read_memory_rpc_repo_state(&engine.repo,&mut cache).unwrap();assert!(next.dirty);assert_eq!(next.dirty_paths,1);assert_eq!(state.system_tokens_estimate,next.system_tokens_estimate);assert_eq!(cache.len(),1);}
    #[test]fn journal_counts_floor_and_pending_is_or(){let root=tempfile::tempdir().unwrap();let mut context=context(root.path());context.ledger.pending_compaction=true;assert!(read_memory_rpc_journal_state(&context,"session").pending_compaction);let dir=context.identity_paths.transcripts.join("session");std::fs::create_dir_all(&dir).unwrap();std::fs::write(dir.join("state.json"),r#"{"steps_since_last_successful_reflection":3.9,"pending_compaction":false,"total_completed_steps":-1,"reflected_completed_steps":"2"}"#).unwrap();let state=read_memory_rpc_journal_state(&context,"session");assert_eq!(state,MemoryRpcJournalState{backlog_steps:3.0,pending_compaction:true,total_steps:0.0,reflected_steps:0.0});for malformed in ["{","[]","null"]{std::fs::write(dir.join("state.json"),malformed).unwrap();assert_eq!(read_memory_rpc_journal_state(&context,"session"),MemoryRpcJournalState{pending_compaction:true,..Default::default()});}}
}
