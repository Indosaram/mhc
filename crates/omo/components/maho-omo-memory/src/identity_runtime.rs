use std::sync::{Arc,atomic::{AtomicU64,Ordering}};
use memory_core::{identity::resolve::{MemoryIdentity,sanitize_to_slug},journal::store::{TranscriptJournal,TranscriptJournalOptions},reflection::{ReflectionReservationStore,ReflectionReservationStoreOptions}};
use crate::context::MemoryIdentityContext;
static RUN_COUNTER:AtomicU64=AtomicU64::new(0);
#[derive(Default)]pub struct IdentitySandbox{built:Option<crate::sandbox::SandboxTransform>}
pub struct IdentitySandboxInput<'a>{pub identity:&'a MemoryIdentityContext,pub policy:crate::sandbox::SandboxPolicy,pub agent_dir:&'a std::path::Path,pub platform:&'a str}
impl IdentitySandbox{
    pub fn apply(&mut self,input:IdentitySandboxInput<'_>,mut args:crate::worker::spawn_types::ReflectionSpawnArgs,which:&dyn Fn(&str)->Option<String>,warn:impl FnOnce(&str))->Result<crate::worker::spawn_types::ReflectionSpawnArgs,String>{
        let IdentitySandboxInput{identity,policy,agent_dir,platform}=input;
        if self.built.is_none(){
            let mut writes=vec![identity.identity_paths.reflection.clone(),agent_dir.into()];
            if let Some(config)=args.env.get("XDG_CONFIG_HOME"){writes.push(config.into());}
            let transform=crate::sandbox::build_sandbox_transform(&crate::sandbox::ReflectionSandboxInput{policy,worktree_dir:&identity.identity_paths.worktrees,git_common_dir:&identity.identity_paths.repo,payload_paths:std::slice::from_ref(&identity.identity_paths.transcripts),runtime_writes:&writes,foreign_roots:&[],command:&args.command,env:&args.env,platform},which).map_err(|error|error.to_string())?;
            if let Some(warning)=&transform.warning{warn(warning);}
            self.built=Some(transform);
        }
        let transformed=self.built.as_ref().ok_or("Missing identity sandbox")?.apply(crate::sandbox_contracts::SandboxSpawnArgs{command:args.command,args:args.args,cwd:args.cwd,env:args.env});
        args.command=transformed.command;args.args=transformed.args;args.cwd=transformed.cwd;args.env=transformed.env;Ok(args)
    }
}

pub fn as_memory_identity(context:&MemoryIdentityContext)->MemoryIdentity{
    MemoryIdentity{id:context.identity.clone(),safe_slug:sanitize_to_slug(&context.identity),paths:context.identity_paths.clone()}
}
pub fn create_identity_reservation_store(context:&MemoryIdentityContext,settings:&serde_json::Value)->Result<Arc<ReflectionReservationStore>,String>{
    let config=crate::trigger_wiring::resolve_reflection_trigger_config(settings,Some(&context.identity))?.config;
    let transcripts=context.identity_paths.transcripts.clone();
    Ok(Arc::new(ReflectionReservationStore::new(ReflectionReservationStoreOptions{
        identity:as_memory_identity(context),config,
        get_journal:Arc::new(move |conversation|Ok(TranscriptJournal::new(TranscriptJournalOptions::new(transcripts.join(conversation))))),
        create_run_id:Some(Arc::new(||format!("reflection-run-{}",RUN_COUNTER.fetch_add(1,Ordering::Relaxed)+1))),
        now_iso:None,launcher_identity:None,
    })))
}
impl crate::worker::runner_types::ReflectionReservationPort for ReflectionReservationStore{
    fn read_state(&self)->Result<memory_core::reflection::ReservationState,String>{ReflectionReservationStore::read_state(self).map_err(|error|error.to_string())}
    fn complete(&self,run:&str,outcome:memory_core::reflection::ReflectionOutcome)->Result<memory_core::reflection::CompletionResult,String>{ReflectionReservationStore::complete(self,run,outcome).map_err(|error|error.to_string())}
}
#[cfg(test)]mod tests{
    use super::*;
    #[test]fn native_store_reserves_manual_run_and_adapts_state(){let root=tempfile::tempdir().unwrap();let context=MemoryIdentityContext::new("agent".into(),memory_core::identity::layout::build_identity_paths(root.path(),"agent"),crate::binding::MemorySessionBinding{identity:"agent".into(),repo_path_hash:"hash".into(),bound_at:0.0});let settings=crate::reflection_settings::resolve_memory_settings(None).unwrap();let store=create_identity_reservation_store(&context,&settings).unwrap();let result=store.evaluate("session",memory_core::reflection::ReflectionEvent::Manual{focus:None,recent_n:None,conversation_ids:None}).unwrap().unwrap();assert_eq!(result.status,"active");assert!(result.run.run_id.starts_with("reflection-run-"));let state=crate::worker::runner_types::ReflectionReservationPort::read_state(store.as_ref()).unwrap();assert_eq!(state.active.unwrap().run_id,result.run.run_id);}
    #[test]fn identity_preserves_paths_and_normalizes_slug(){let root=tempfile::tempdir().unwrap();let context=MemoryIdentityContext::new("Agent One".into(),memory_core::identity::layout::build_identity_paths(root.path(),"Agent One"),crate::binding::MemorySessionBinding{identity:"Agent One".into(),repo_path_hash:"hash".into(),bound_at:0.0});let identity=as_memory_identity(&context);assert_eq!(identity.id,"Agent One");assert_eq!(identity.safe_slug,sanitize_to_slug("Agent One"));assert_eq!(identity.paths.repo,context.identity_paths.repo);}
    #[test]fn invalid_trigger_settings_fail_before_store_construction(){let root=tempfile::tempdir().unwrap();let context=MemoryIdentityContext::new("agent".into(),memory_core::identity::layout::build_identity_paths(root.path(),"agent"),crate::binding::MemorySessionBinding{identity:"agent".into(),repo_path_hash:"hash".into(),bound_at:0.0});assert!(create_identity_reservation_store(&context,&serde_json::Value::Null).is_err());}
}
