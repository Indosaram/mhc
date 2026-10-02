use std::{collections::BTreeMap,path::Path};
use memory_core::{git::GitMemoryRepo,identity::resolve::MemoryIdentity,reflection::{ReflectionWorktree,ReservedRun}};
use super::{memory_model_attempts::{MemoryModelAttemptError,ReflectionModelCandidate},model_preflight::{ConfigSource,Launcher,ModelPreflight},resolve_model::ReflectionModelResolution,run_finalization_types::RunFinalizationContext,runner_types::{ExecutionResult,ReflectionRunResult},spawn_supervisor::ReflectionChildOptions};

pub enum ReflectionExecutionResult{Finalized(Box<ReflectionRunResult>),Failed(ExecutionResult)}
pub struct ReflectionExecutionInput<'a>{
    pub run:&'a ReservedRun,pub resolution:&'a ReflectionModelResolution,pub identity:&'a MemoryIdentity,
    pub config:&'a serde_json::Value,pub env:&'a BTreeMap<String,String>,pub sources:&'a [ConfigSource],pub launch:&'a Launcher,
    pub parent_session_file:Option<&'a Path>,pub parent_cwd:Option<&'a Path>,pub merge_policy:&'a str,
    pub sandbox:Option<&'a dyn Fn(super::spawn_types::ReflectionSpawnArgs)->Result<super::spawn_types::ReflectionSpawnArgs,String>>,
    pub hard_deadline_at:f64,pub options:ReflectionChildOptions<'a>,pub finalization:&'a RunFinalizationContext<'a>,
}

pub async fn execute_reflection_run(
    cache:&mut ModelPreflight,input:ReflectionExecutionInput<'_>,
    create:impl FnOnce(&GitMemoryRepo,&ReservedRun)->Result<ReflectionWorktree,String>,
    append_launched:impl FnOnce()->Result<(),String>,warn:impl FnOnce(&str),
    terminal_gate:impl FnOnce(&mut dyn FnMut()->Result<Option<super::run_finalization_types::ReservationRunResult>,String>)->Result<Option<super::run_finalization_types::ReservationRunResult>,String>,
)->Result<ReflectionExecutionResult,String>{
    let engine=crate::engine_session::prepare_memory_engine_session(&input.identity.id,&input.identity.paths,Default::default()).map_err(|error|error.to_string())?;
    let worktree=match create(&engine.repo,input.run){Ok(worktree)=>worktree,Err(error)=>return finalize_execution_failure(&input,None,MemoryModelAttemptError::Launch(error),terminal_gate)};
    let ReflectionModelResolution::Resolved{model,thinking,fallbacks,category,..}=input.resolution else{return finalize_execution_failure(&input,Some(&worktree),MemoryModelAttemptError::Launch("reflection child launch requires a resolved model".into()),terminal_gate);};
    let launched=std::cell::RefCell::new(Some(append_launched));
    let worktree_ref=&worktree;let input_ref=&input;
    let result=super::memory_launch_preflight::resolve_and_preflight_memory_launch_typed(cache,super::memory_launch_preflight::MemoryLaunchPreflightInput{first:ReflectionModelCandidate{model:model.clone(),thinking:thinking.clone()},rest:fallbacks,launch:input.launch,env:input.env,env_flag:"SENPI_MEMORY_REFLECTION",config_sources:input.sources,surface_name:"reflection",now_ms:input.options.launched_at},warn,|candidate,attempt,next_attempt|{
        let launched=&launched;
        async move{
            let args=super::reflection_spawn_input::prepare_reflection_candidate_spawn(super::reflection_spawn_input::ReflectionCandidateSpawnInput{parent_session_file:input_ref.parent_session_file,parent_cwd:input_ref.parent_cwd,run:input_ref.run,worktree:worktree_ref,merge_policy:input_ref.merge_policy,category,candidate:&candidate,attempt:u32::try_from(attempt).map_err(|error|error.to_string())?,hard_deadline_at:input_ref.hard_deadline_at,next_attempt,config:input_ref.config,identity:input_ref.identity,env:input_ref.env.clone(),launch:input_ref.launch.clone(),now_ms:input_ref.options.launched_at as f64}).map_err(|error|error.to_string())?;
            if let Some(append)=launched.borrow_mut().take(){append()?;}
            let args=match input_ref.sandbox{Some(sandbox)=>sandbox(args)?,None=>args};
            super::spawn_supervisor::run_reflection_child(&args,ReflectionChildOptions{termination_grace_ms:input_ref.options.termination_grace_ms,max_output_bytes:input_ref.options.max_output_bytes,supervisor_command:input_ref.options.supervisor_command,supervisor_args:input_ref.options.supervisor_args,launched_at:input_ref.options.launched_at}).await
        }
    }).await;
    if let Err(error)=result{return finalize_execution_failure(&input,Some(&worktree),error,terminal_gate);}
    let run_dir=input.identity.paths.reflection.join("runs").join(&input.run.run_id);
    let ledger=super::reservation_run_ledger::parse_reservation_run_ledger(super::run_artifacts::read_run_json(&run_dir.join("ledger.json")).map_err(|error|error.to_string())?).map_err(|error|error.to_string())?;
    super::runner_finalization_result::require_finalized_result(super::run_finalization::finalize_recorded_outcome(input.finalization,&run_dir,&ledger)?).map(|result|ReflectionExecutionResult::Finalized(Box::new(result)))
}

fn finalize_execution_failure(
    input:&ReflectionExecutionInput<'_>,worktree:Option<&ReflectionWorktree>,error:MemoryModelAttemptError,
    gate:impl FnOnce(&mut dyn FnMut()->Result<Option<super::run_finalization_types::ReservationRunResult>,String>)->Result<Option<super::run_finalization_types::ReservationRunResult>,String>,
)->Result<ReflectionExecutionResult,String>{
    let dir=input.identity.paths.reflection.join("runs").join(&input.run.run_id);
    if dir.join("ledger.json").exists(){
        let ledger=super::reservation_run_ledger::parse_reservation_run_ledger(super::run_artifacts::read_run_json(&dir.join("ledger.json")).map_err(|error|error.to_string())?).map_err(|error|error.to_string())?;
        let finalized=if matches!(error,MemoryModelAttemptError::Exhausted(_)){super::run_finalization::override_failed_reservation_run(input.finalization,&dir,&ledger,error.to_string(),gate)?}else{super::run_finalization::fail_reservation_run(input.finalization,&dir,&ledger,false,Some(error.to_string()),gate)?};
        return super::runner_finalization_result::require_finalized_result(finalized).map(|result|ReflectionExecutionResult::Finalized(Box::new(result)));
    }
    let cleanup=worktree.map(memory_core::reflection::cleanup_reflection_worktree);
    let failed=cleanup.is_some_and(|cleanup|!cleanup.worktree_removed||!cleanup.branch_removed);
    Ok(ReflectionExecutionResult::Failed(ExecutionResult{outcome:"failed".into(),reason:Some(if failed{"cleanup_failed"}else{"spawn_failed"}.into()),detail:Some(error.to_string()),model:None,thinking:None}))
}

#[cfg(all(test,unix))]
mod tests{
    use super::*;
    struct Inactive;
    impl super::super::runner_types::ReflectionReservationPort for Inactive{
        fn read_state(&self)->Result<memory_core::reflection::ReservationState,String>{Ok(Default::default())}
        fn complete(&self,_:&str,_:memory_core::reflection::ReflectionOutcome)->Result<memory_core::reflection::CompletionResult,String>{panic!("inactive reservation")}
    }
    #[tokio::test]async fn real_worktree_model_chain_publishes_and_finalizes_no_changes(){
        let root=tempfile::tempdir().unwrap();let identity=MemoryIdentity{id:"agent".into(),safe_slug:"agent".into(),paths:memory_core::identity::layout::build_identity_paths(root.path(),"agent")};
        let run=ReservedRun{run_id:"run".into(),request:memory_core::reflection::ReflectionRequest{trigger:memory_core::reflection::ReflectionTrigger::Manual,origin:None,conversation_ids:vec![],snapshots:vec![],focus:None,recent_n:None,target_doc:None},reserved_at:None,launcher_pid:None,launcher_hostname:None,launcher_process_start:None};
        let resolution=ReflectionModelResolution::Resolved{model:"p/m".into(),thinking:None,fallbacks:vec![],category:"quick".into(),source:None};let env=BTreeMap::new();let config=serde_json::json!({"memory":{}});let launch=Launcher{command:"/bin/sh".into(),prefix_args:vec!["-c".into(),"printf 'p/m\\n'".into()]};
        let supervisor_args=vec!["-c".into(),"printf '%s' '{\"version\":1,\"runId\":\"run\",\"attempt\":1,\"finishedAt\":\"now\",\"childExit\":{\"code\":0,\"signal\":null},\"timedOut\":false}' > \"$0/outcome.json\"; rm \"$0/launch.json\"".into()];
        let context=RunFinalizationContext{identity:&identity,reservation:&Inactive,launch:None,now_ms:&||0};let launched=std::cell::Cell::new(0);
        let result=execute_reflection_run(&mut ModelPreflight::default(),ReflectionExecutionInput{run:&run,resolution:&resolution,identity:&identity,config:&config,env:&env,sources:&[],launch:&launch,parent_session_file:None,parent_cwd:None,merge_policy:"auto",sandbox:None,hard_deadline_at:chrono::Utc::now().timestamp_millis() as f64+60000.0,options:ReflectionChildOptions{termination_grace_ms:100.0,max_output_bytes:1024,supervisor_command:Path::new("/bin/sh"),supervisor_args:&supervisor_args,launched_at:0},finalization:&context},|repo,run|{
            let exec=memory_core::git::exec::create_git_exec(Default::default());memory_core::reflection::create_reflection_worktree(repo,&run.run_id,&identity.paths.worktrees,exec.as_ref(),None).map_err(|error|error.to_string())
        },||{launched.set(launched.get()+1);Ok(())},|error|panic!("{error}"),|operation|operation()).await.unwrap();
        let ReflectionExecutionResult::Finalized(result)=result else{panic!("must finalize")};assert_eq!(result.outcome,"no_changes");assert_eq!(launched.get(),1);assert!(identity.paths.reflection.join("runs/run/final.json").exists());
    }
}
