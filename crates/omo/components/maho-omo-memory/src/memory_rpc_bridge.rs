use std::collections::BTreeMap;
use memory_core::git::{GitMemoryRepo,GitMemoryRepoOptions,errors::GitError};
use crate::{context::MemoryIdentityContext,status_active_runs::ActiveReflectionRun};
pub const MEMORY_UPDATED_RPC_EVENT:&str="omo.memory.updated";
pub const MEMORY_STATUS_RPC_METHOD:&str="omo.memory.status";
pub trait MemoryRpcBridgeDeps{
    fn resolve_context(&self,session:&str)->Option<&MemoryIdentityContext>;
    fn active_run(&self,identity:&str)->Option<&ActiveReflectionRun>;
}
pub trait MemoryRpcEmitter{fn emit(&mut self,name:&str,data:&serde_json::Value);}
#[derive(Default)]
pub struct MemoryRpcBridge{session_id:Option<String>,last_snapshot:Option<String>,disposed:bool,repos:BTreeMap<std::path::PathBuf,GitMemoryRepo>,token_estimates:BTreeMap<String,usize>}
impl MemoryRpcBridge{
    pub fn attach(&mut self,session:&str){if self.disposed{return;}self.session_id=Some(session.into());self.last_snapshot=None;}
    pub fn detach(&mut self){self.session_id=None;self.last_snapshot=None;}
    pub fn dispose(&mut self){self.disposed=true;self.detach();}
    fn build_snapshot(&mut self,deps:&dyn MemoryRpcBridgeDeps,now:i64)->Result<Option<serde_json::Value>,GitError>{
        if self.disposed{return Ok(None);}
        let Some(session)=&self.session_id else{return Ok(None);};let Some(context)=deps.resolve_context(session)else{return Ok(None);};
        let repo=match self.repos.entry(context.identity_paths.repo.clone()){
            std::collections::btree_map::Entry::Occupied(entry)=>entry.into_mut(),
            std::collections::btree_map::Entry::Vacant(entry)=>entry.insert(GitMemoryRepo::new(GitMemoryRepoOptions::new(&context.identity_paths.repo,"omo-memory-rpc"))?),
        };
        let repo_state=crate::memory_rpc_snapshot_state::read_memory_rpc_repo_state(repo,&mut self.token_estimates)?;
        let journal=crate::memory_rpc_snapshot_state::read_memory_rpc_journal_state(context,session);
        let health=crate::memory_rpc_snapshot_state::read_memory_rpc_health(context,now);
        let mut reflection=serde_json::json!({"backlogSteps":journal.backlog_steps,"pendingCompaction":journal.pending_compaction,"consecutiveFailures":health["streak"]});
        if let Some(run)=deps.active_run(&context.identity){let mut value=serde_json::json!({"runId":run.run_id,"trigger":run.details.trigger,"category":run.details.category,"startedAt":run.details.started_at});if let Some(model)=&run.details.model{value["model"]=model.clone().into();}reflection["activeRun"]=value;}
        if let Some(last)=health.get("lastOutcome"){reflection["lastOutcome"]=last.clone();}
        if let Some(fingerprint)=health.get("fingerprint"){reflection["lastFailureFingerprint"]=fingerprint.clone();}
        Ok(Some(serde_json::json!({"schemaVersion":1,"identity":context.identity,"repo":repo_state,"reflection":reflection,"journal":{"sessionId":session,"totalSteps":journal.total_steps,"reflectedSteps":journal.reflected_steps}})))
    }
    pub fn sync(&mut self,deps:&dyn MemoryRpcBridgeDeps,emitter:Option<&mut dyn MemoryRpcEmitter>,now:i64)->Result<(),GitError>{
        let Some(emitter)=emitter else{return Ok(());};let Some(snapshot)=self.build_snapshot(deps,now)?else{return Ok(());};let fingerprint=snapshot.to_string();if self.last_snapshot.as_ref()==Some(&fingerprint){return Ok(());}self.last_snapshot=Some(fingerprint);emitter.emit(MEMORY_UPDATED_RPC_EVENT,&snapshot);Ok(())
    }
    pub fn status(&mut self,deps:&dyn MemoryRpcBridgeDeps,now:i64)->Result<serde_json::Value,GitError>{Ok(self.build_snapshot(deps,now)?.unwrap_or_else(||serde_json::json!({"kind":"unavailable","reason":"No bound memory session."})))}
}
#[cfg(test)]
mod tests{
    use super::*;
    struct Deps{context:Option<MemoryIdentityContext>}
    impl MemoryRpcBridgeDeps for Deps{fn resolve_context(&self,_:&str)->Option<&MemoryIdentityContext>{self.context.as_ref()}fn active_run(&self,_:&str)->Option<&ActiveReflectionRun>{None}}
    #[derive(Default)]struct Emitter(Vec<serde_json::Value>);
    impl MemoryRpcEmitter for Emitter{fn emit(&mut self,name:&str,data:&serde_json::Value){assert_eq!(name,MEMORY_UPDATED_RPC_EVENT);self.0.push(data.clone());}}
    fn deps(root:&std::path::Path)->Deps{let paths=memory_core::identity::layout::build_identity_paths(root,"agent");crate::engine_session::prepare_memory_engine_session("agent",&paths,Default::default()).unwrap();Deps{context:Some(MemoryIdentityContext::new("agent".into(),paths,crate::binding::MemorySessionBinding{identity:"agent".into(),repo_path_hash:"hash".into(),bound_at:0.0}))}}
    #[test]fn real_snapshot_dedupes_and_pull_does_not_emit(){let root=tempfile::tempdir().unwrap();let deps=deps(root.path());let mut bridge=MemoryRpcBridge::default();let mut emitter=Emitter::default();bridge.attach("session");for _ in 0..3{bridge.sync(&deps,Some(&mut emitter),0).unwrap();}assert_eq!(emitter.0.len(),1);let snapshot=bridge.status(&deps,0).unwrap();assert_eq!(snapshot,emitter.0[0]);assert_eq!(snapshot["identity"],"agent");assert_eq!(snapshot["journal"]["sessionId"],"session");assert_eq!(emitter.0.len(),1);}
    #[test]fn no_rpc_does_no_git_and_unbound_is_unavailable(){let deps=Deps{context:None};let mut bridge=MemoryRpcBridge::default();assert_eq!(bridge.status(&deps,0).unwrap()["kind"],"unavailable");bridge.attach("session");bridge.sync(&deps,None,0).unwrap();assert!(bridge.repos.is_empty());let mut emitter=Emitter::default();bridge.sync(&deps,Some(&mut emitter),0).unwrap();assert!(emitter.0.is_empty());assert!(bridge.repos.is_empty());}
    #[test]fn detach_reattach_and_dispose(){let root=tempfile::tempdir().unwrap();let deps=deps(root.path());let mut bridge=MemoryRpcBridge::default();let mut emitter=Emitter::default();bridge.attach("session");bridge.sync(&deps,Some(&mut emitter),0).unwrap();bridge.detach();bridge.sync(&deps,Some(&mut emitter),0).unwrap();assert_eq!(emitter.0.len(),1);bridge.attach("session");bridge.sync(&deps,Some(&mut emitter),0).unwrap();assert_eq!(emitter.0.len(),2);bridge.dispose();bridge.attach("session");bridge.sync(&deps,Some(&mut emitter),0).unwrap();assert_eq!(emitter.0.len(),2);assert_eq!(bridge.status(&deps,0).unwrap()["kind"],"unavailable");}
    #[test]fn journal_change_pushes_fresh_snapshot(){let root=tempfile::tempdir().unwrap();let deps=deps(root.path());let mut bridge=MemoryRpcBridge::default();let mut emitter=Emitter::default();bridge.attach("session");bridge.sync(&deps,Some(&mut emitter),0).unwrap();let path=deps.context.as_ref().unwrap().identity_paths.transcripts.join("session");std::fs::create_dir_all(&path).unwrap();std::fs::write(path.join("state.json"),r#"{"total_completed_steps":42,"reflected_completed_steps":28,"steps_since_last_successful_reflection":14,"pending_compaction":true}"#).unwrap();bridge.sync(&deps,Some(&mut emitter),0).unwrap();assert_eq!(emitter.0.len(),2);assert_eq!(emitter.0[1]["reflection"]["backlogSteps"],14.0);assert_eq!(emitter.0[1]["journal"]["totalSteps"],42.0);}
}
