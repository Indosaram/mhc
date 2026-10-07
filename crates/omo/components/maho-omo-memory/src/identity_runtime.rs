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
            let mut writes=vec![identity.identity_paths.reflection_sessions.clone(),identity.identity_paths.reflection.clone(),agent_dir.into()];
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
pub struct MemoryIdentityRuntime{
    pub identity:MemoryIdentityContext,pub store:Arc<ReflectionReservationStore>,
    pub runner:crate::worker::runner::SenpiSubprocessRunner,pub sandbox:IdentitySandbox,
}
pub fn create_identity_runtime(identity:MemoryIdentityContext,settings:&serde_json::Value)->Result<MemoryIdentityRuntime,String>{
    let store=create_identity_reservation_store(&identity,settings)?;
    Ok(MemoryIdentityRuntime{identity,store,runner:Default::default(),sandbox:Default::default()})
}
impl MemoryIdentityRuntime{
    pub async fn launch<F:std::future::Future<Output=Result<crate::worker::runner_types::ReflectionRunResult,String>>>(
        &self,mut run:memory_core::reflection::ReservedRun,mut launch:impl FnMut(memory_core::reflection::ReservedRun)->F,warn:impl FnOnce(&str),
    ){
        loop{match launch(run).await{
            Ok(result)=>match result.launch{Some(next)=>run=next,None=>return},
            Err(error)=>{warn(&error);return;}
        }}
    }
    pub async fn reconcile(&self,now:&dyn Fn()->i64,recovery:&dyn crate::worker::run_reconciliation::ReflectionRecoveryPort,launch:&dyn Fn(&memory_core::reflection::ReservedRun))->Result<Vec<(String,String)>,String>{
        let identity=as_memory_identity(&self.identity);
        crate::worker::run_reconciliation::reconcile_reflection_runs_with_options(&crate::worker::run_finalization_types::RunFinalizationContext{identity:&identity,reservation:self.store.as_ref(),launch:Some(launch),now_ms:now},recovery,None,true).await
    }
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
    fn read_state_with_wait(&self,wait_timeout_ms:Option<u64>)->Result<memory_core::reflection::ReservationState,memory_core::reflection::ReservationError>{ReflectionReservationStore::read_state_with_wait(self,wait_timeout_ms)}
    fn complete(&self,run:&str,outcome:memory_core::reflection::ReflectionOutcome)->Result<memory_core::reflection::CompletionResult,String>{ReflectionReservationStore::complete(self,run,outcome).map_err(|error|error.to_string())}
}
#[cfg(test)]mod tests{
    use super::*;
    struct SweepRecovery;
    impl crate::worker::run_reconciliation::ReflectionRecoveryPort for SweepRecovery {
        fn hostname(&self)->String{"host".into()}
        fn pid_liveness(&self,_:u32)->memory_core::locks::ProcessLiveness{memory_core::locks::ProcessLiveness::Dead}
        fn process_start(&self,_:u32)->Option<String>{None}
        fn classify(&self,_:Option<u64>,_:Option<Option<&str>>)->crate::worker::run_liveness::RunProcessVerdict{crate::worker::run_liveness::RunProcessVerdict::Absent}
        fn wait_outcome<'a>(&'a self,_:&'a std::path::Path,_:f64)->crate::worker::run_reconciliation::ReconcileFuture<'a>{Box::pin(async{Ok(())})}
        fn wait_until(&self,_:f64)->crate::worker::run_reconciliation::ReconcileFuture<'_>{Box::pin(async{Ok(())})}
        fn signal_group(&self,_:u64,_:&str)->Result<(),String>{Ok(())}
        fn fail(&self,_:&std::path::Path,_:&crate::worker::reservation_run_ledger::ReservationRunLedger,_:bool)->Result<Option<crate::worker::run_finalization_types::ReservationRunResult>,String>{Ok(None)}
        fn abandon(&self,_:&std::path::Path,_:&crate::worker::reservation_run_ledger::ReservationRunLedger)->Result<Option<crate::worker::run_finalization_types::ReservationRunResult>,String>{Ok(None)}
    }

    #[tokio::test]
    async fn the_component_reconcile_reclaims_an_orphan_and_preserves_the_reserved_live_run() {
        let root=tempfile::tempdir().unwrap();
        let context=MemoryIdentityContext::new("agent".into(),memory_core::identity::layout::build_identity_paths(root.path(),"agent"),crate::binding::MemorySessionBinding{identity:"agent".into(),repo_path_hash:"hash".into(),bound_at:0.0});
        std::fs::create_dir_all(&context.identity_paths.worktrees).unwrap();
        let repo=memory_core::git::GitMemoryRepo::open(&context.identity_paths.repo,"agent").unwrap();
        repo.init(Some(memory_core::git::InitializeGitRepoOptions::default())).unwrap();
        let exec=repo.exec();
        let settings=crate::reflection_settings::resolve_memory_settings(None).unwrap();
        let runtime=create_identity_runtime(context.clone(),&settings).unwrap();
        let reserved=runtime.store.evaluate("session",memory_core::reflection::ReflectionEvent::Manual{focus:None,recent_n:None,conversation_ids:None}).unwrap().expect("reserved run");
        assert_eq!(reserved.status,"active");
        let live=memory_core::reflection::create_reflection_worktree(&repo,&reserved.run.run_id,&context.identity_paths.worktrees,exec.as_ref(),None).unwrap();
        let orphan=memory_core::reflection::create_reflection_worktree(&repo,"run-gone",&context.identity_paths.worktrees,exec.as_ref(),None).unwrap();

        let now=||4_000_000_000_000_i64;
        let launch=|_run:&memory_core::reflection::ReservedRun|{};
        let results=runtime.reconcile(&now,&SweepRecovery,&launch).await;

        assert_eq!(results,Ok(Vec::new()));
        assert!(!orphan.dir.exists());
        let gone=exec.run_in(&repo.dir,&["show-ref","--verify",&format!("refs/heads/{}",orphan.branch)]).unwrap();
        assert_ne!(gone.code,0);
        assert!(live.dir.exists());
        let kept=exec.run_in(&repo.dir,&["show-ref","--verify",&format!("refs/heads/{}",live.branch)]).unwrap();
        assert_eq!(kept.code,0);
        let cleanup=memory_core::reflection::discard_reflection_worktree(&repo,&live.dir,&live.branch,exec.as_ref());
        assert!(cleanup.worktree_removed&&cleanup.branch_removed);
    }

    #[tokio::test]
    async fn the_component_reconcile_leaves_an_in_grace_orphan_untouched() {
        let root=tempfile::tempdir().unwrap();
        let context=MemoryIdentityContext::new("agent".into(),memory_core::identity::layout::build_identity_paths(root.path(),"agent"),crate::binding::MemorySessionBinding{identity:"agent".into(),repo_path_hash:"hash".into(),bound_at:0.0});
        std::fs::create_dir_all(&context.identity_paths.worktrees).unwrap();
        let repo=memory_core::git::GitMemoryRepo::open(&context.identity_paths.repo,"agent").unwrap();
        repo.init(Some(memory_core::git::InitializeGitRepoOptions::default())).unwrap();
        let exec=repo.exec();
        let young=memory_core::reflection::create_reflection_worktree(&repo,"run-young",&context.identity_paths.worktrees,exec.as_ref(),None).unwrap();
        let settings=crate::reflection_settings::resolve_memory_settings(None).unwrap();
        let runtime=create_identity_runtime(context.clone(),&settings).unwrap();

        let now=||0_i64;
        let launch=|_run:&memory_core::reflection::ReservedRun|{};
        let results=runtime.reconcile(&now,&SweepRecovery,&launch).await;

        assert_eq!(results,Ok(Vec::new()));
        assert!(young.dir.exists());
        let cleanup=memory_core::reflection::discard_reflection_worktree(&repo,&young.dir,&young.branch,exec.as_ref());
        assert!(cleanup.worktree_removed&&cleanup.branch_removed);
    }
    #[test]fn native_store_reserves_manual_run_and_adapts_state(){let root=tempfile::tempdir().unwrap();let context=MemoryIdentityContext::new("agent".into(),memory_core::identity::layout::build_identity_paths(root.path(),"agent"),crate::binding::MemorySessionBinding{identity:"agent".into(),repo_path_hash:"hash".into(),bound_at:0.0});let settings=crate::reflection_settings::resolve_memory_settings(None).unwrap();let store=create_identity_reservation_store(&context,&settings).unwrap();let result=store.evaluate("session",memory_core::reflection::ReflectionEvent::Manual{focus:None,recent_n:None,conversation_ids:None}).unwrap().unwrap();assert_eq!(result.status,"active");assert!(result.run.run_id.starts_with("reflection-run-"));let state=crate::worker::runner_types::ReflectionReservationPort::read_state(store.as_ref()).unwrap();assert_eq!(state.active.unwrap().run_id,result.run.run_id);}
    #[test]fn identity_preserves_paths_and_normalizes_slug(){let root=tempfile::tempdir().unwrap();let context=MemoryIdentityContext::new("Agent One".into(),memory_core::identity::layout::build_identity_paths(root.path(),"Agent One"),crate::binding::MemorySessionBinding{identity:"Agent One".into(),repo_path_hash:"hash".into(),bound_at:0.0});let identity=as_memory_identity(&context);assert_eq!(identity.id,"Agent One");assert_eq!(identity.safe_slug,sanitize_to_slug("Agent One"));assert_eq!(identity.paths.repo,context.identity_paths.repo);}
    #[test]fn invalid_trigger_settings_fail_before_store_construction(){let root=tempfile::tempdir().unwrap();let context=MemoryIdentityContext::new("agent".into(),memory_core::identity::layout::build_identity_paths(root.path(),"agent"),crate::binding::MemorySessionBinding{identity:"agent".into(),repo_path_hash:"hash".into(),bound_at:0.0});assert!(create_identity_reservation_store(&context,&serde_json::Value::Null).is_err());}
}
