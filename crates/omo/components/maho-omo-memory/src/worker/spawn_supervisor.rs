use std::{collections::BTreeMap,path::Path};
use super::{run_artifacts::{RunLaunchManifest,RunOutcome,RunKind,RunAttempt,write_run_json_atomic,read_run_json,run_outcome_matches_ledger},memory_model_attempts::ReflectionChildResult,run_sentinel::{wait_for_run_completion,SentinelWaitResult},spawn_supervisor_support::{read_tail,is_node_signal}};

pub struct SupervisedChildInput<'a> {
    pub run_dir:&'a Path,pub run_id:&'a str,pub attempt:u32,pub model:&'a str,pub thinking:Option<&'a str>,pub next_attempt:Option<RunAttempt>,pub kind:RunKind,
    pub command:&'a str,pub args:&'a [String],pub cwd:&'a Path,pub env:&'a BTreeMap<String,String>,pub hard_deadline_at:f64,pub termination_grace_ms:f64,pub max_output_bytes:usize,
    pub supervisor_command:&'a Path,pub supervisor_args:&'a [String],pub ledger:serde_json::Map<String,serde_json::Value>,
}
pub struct FactsChildOptions<'a> {
    pub termination_grace_ms:f64,pub max_output_bytes:usize,pub supervisor_command:&'a Path,pub supervisor_args:&'a [String],pub batch_id:&'a str,pub queued:&'a [crate::facts_failure_recording::FactsQueuedKey],pub launched_at:i64,
}
pub struct ReflectionChildOptions<'a> {
    pub termination_grace_ms:f64,pub max_output_bytes:usize,pub supervisor_command:&'a Path,pub supervisor_args:&'a [String],pub launched_at:i64,
}
pub async fn run_reflection_child(args:&super::spawn_types::ReflectionSpawnArgs,options:ReflectionChildOptions<'_>)->Result<ReflectionChildResult,String> {
    if options.termination_grace_ms<0.0||options.max_output_bytes==0{return Err("reflection spawn limits are invalid".into());}
    let metadata=super::spawn_metadata::require_run_metadata(args).map_err(|error|format!("{error:?}"))?;
    if !args.hard_deadline_at.is_finite()||args.hard_deadline_at<=0.0{return Err("reflection deadline must be positive".into());}
    let started=chrono::DateTime::from_timestamp_millis(options.launched_at).ok_or("Invalid launch timestamp")?.to_rfc3339_opts(chrono::SecondsFormat::Millis,true);
    let worktree=metadata.worktree;
    let mut ledger=serde_json::json!({"version":1,"runId":metadata.run_id,"kind":metadata.kind,"category":args.category,"conversationIds":args.conversation_ids,"trigger":metadata.trigger,"startedAt":started,"attempt":args.attempt,"model":args.model,"hardDeadlineAt":args.hard_deadline_at,"terminationGraceMs":options.termination_grace_ms,"deadlineAt":args.hard_deadline_at+options.termination_grace_ms,"mergePolicy":metadata.merge_policy,"worktreeDir":worktree.dir,"worktreeBranch":worktree.branch,"baseSha":worktree.base_commit_sha,"gitFilePath":worktree.git_file_path,"gitFileSnapshot":worktree.git_file_snapshot,"commonConfigPath":worktree.common_config_path,"commonConfigSnapshot":worktree.common_config_snapshot});
    if metadata.kind=="dream" {ledger["origin"]=metadata.origin.into();}
    if let Some(target)=metadata.target_doc {ledger["targetDoc"]=target.into();}
    let kind=if metadata.kind=="dream"{RunKind::Dream}else{RunKind::Reflection};
    run_supervised_child(SupervisedChildInput {run_dir:&args.paths.session_dir,run_id:metadata.run_id,attempt:args.attempt,model:&args.model,thinking:args.thinking.as_deref(),next_attempt:args.next_attempt.clone(),kind,command:&args.command,args:&args.args,cwd:&args.cwd,env:&args.env,hard_deadline_at:args.hard_deadline_at,termination_grace_ms:options.termination_grace_ms,max_output_bytes:options.max_output_bytes,supervisor_command:options.supervisor_command,supervisor_args:options.supervisor_args,ledger:ledger.as_object().ok_or("Invalid reflection ledger")?.clone()}).await
}
pub async fn run_facts_child(args:&super::spawn_types::FactsSpawnArgs,options:FactsChildOptions<'_>)->Result<ReflectionChildResult,String> {
    if options.termination_grace_ms<0.0||options.max_output_bytes==0{return Err("facts spawn limits are invalid".into());}
    if !args.hard_deadline_at.is_finite()||args.hard_deadline_at<=0.0{return Err("facts deadline must be positive".into());}
    let started=chrono::DateTime::from_timestamp_millis(options.launched_at).ok_or("Invalid launch timestamp")?.to_rfc3339_opts(chrono::SecondsFormat::Millis,true);
    let ledger=serde_json::json!({"version":1,"runId":args.run_id,"kind":"facts","startedAt":started,"attempt":args.attempt,"model":args.model,"hardDeadlineAt":args.hard_deadline_at,"terminationGraceMs":options.termination_grace_ms,"deadlineAt":args.hard_deadline_at+options.termination_grace_ms,"batchId":options.batch_id,"queued":options.queued});
    run_supervised_child(SupervisedChildInput {run_dir:&args.paths.run_dir,run_id:&args.run_id,attempt:args.attempt,model:&args.model,thinking:args.thinking.as_deref(),next_attempt:args.next_attempt.clone(),kind:RunKind::Facts,command:&args.command,args:&args.args,cwd:&args.cwd,env:&args.env,hard_deadline_at:args.hard_deadline_at,termination_grace_ms:options.termination_grace_ms,max_output_bytes:options.max_output_bytes,supervisor_command:options.supervisor_command,supervisor_args:options.supervisor_args,ledger:ledger.as_object().ok_or("Invalid facts ledger")?.clone()}).await
}
pub async fn run_supervised_child(input:SupervisedChildInput<'_>)->Result<ReflectionChildResult,String> {
    if input.termination_grace_ms<0.0||input.max_output_bytes==0 {return Err("reflection spawn limits are invalid".into());}
    if !input.hard_deadline_at.is_finite()||input.hard_deadline_at<=0.0 {return Err("reflection deadline must be positive".into());}
    let mut directory=std::fs::DirBuilder::new();directory.recursive(true);
    #[cfg(unix)] {use std::os::unix::fs::DirBuilderExt;directory.mode(0o700);}
    directory.create(input.run_dir).map_err(|error|error.to_string())?;
    let stdout=input.run_dir.join("child-stdout.log");let stderr=input.run_dir.join("child-stderr.log");
    let launch=input.run_dir.join("launch.json");let outcome_path=input.run_dir.join("outcome.json");
    let manifest=RunLaunchManifest {version:1,run_id:input.run_id.into(),attempt:input.attempt,next_attempt:input.next_attempt,kind:input.kind,command:input.command.into(),args:input.args.to_vec(),cwd:input.cwd.to_string_lossy().into_owned(),env:input.env.clone(),hard_deadline_at:input.hard_deadline_at,termination_grace_ms:input.termination_grace_ms,max_output_bytes:input.max_output_bytes,stdout_path:stdout.to_string_lossy().into_owned(),stderr_path:stderr.to_string_lossy().into_owned()};
    let mut ledger=input.ledger;
    ledger.insert("attempt".into(),input.attempt.into());ledger.insert("model".into(),input.model.into());
    if let Some(thinking)=input.thinking {ledger.insert("thinking".into(),thinking.into());}
    ledger.insert("launching".into(),true.into());ledger.insert("hardDeadlineAt".into(),input.hard_deadline_at.into());ledger.insert("deadlineAt".into(),(input.hard_deadline_at+input.termination_grace_ms).into());
    write_run_json_atomic(&input.run_dir.join("ledger.json"),&ledger,0o600).map_err(|error|error.to_string())?;
    write_run_json_atomic(&launch,&manifest,0o600).map_err(|error|error.to_string())?;
    let mut command=tokio::process::Command::new(input.supervisor_command);
    command.args(input.supervisor_args).arg(input.run_dir).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    #[cfg(unix)] {command.process_group(0);}
    let mut supervisor=command.spawn().map_err(|error|error.to_string())?;
    let matching=||read_run_json::<RunOutcome>(&outcome_path).is_ok_and(|outcome|run_outcome_matches_ledger(Some(input.attempt),&outcome));
    let complete=||outcome_path.exists()&&!launch.exists()&&matching();
    let wait=wait_for_run_completion(&outcome_path,&launch,matching,input.hard_deadline_at+input.termination_grace_ms+5000.0,||chrono::Utc::now().timestamp_millis() as f64,None);
    tokio::pin!(wait);
    tokio::select! {
        result=supervisor.wait()=> {
            if !complete() {return Err(match result {Ok(status)=>format!("memory run supervisor exited with {}",status.code().map_or_else(||"unknown status".into(),|code|code.to_string())),Err(error)=>error.to_string()});}
        },
        result=&mut wait=>if result==SentinelWaitResult::Timeout {return Err("memory run supervisor did not publish an outcome before its deadline".into());},
    }
    let outcome:RunOutcome=read_run_json(&outcome_path).map_err(|error|error.to_string())?;
    if !run_outcome_matches_ledger(Some(input.attempt),&outcome) {return Err(format!("memory run outcome attempt {} does not match attempt {}",outcome.attempt.map_or("legacy".into(),|attempt|attempt.to_string()),input.attempt));}
    Ok(ReflectionChildResult {code:outcome.child_exit.code,signal:outcome.child_exit.signal.filter(|signal|is_node_signal(Some(signal))),stdout:read_tail(&stdout,input.max_output_bytes),stderr:read_tail(&stderr,input.max_output_bytes),timed_out:outcome.timed_out})
}

#[cfg(all(test,unix))]
mod tests {
    use super::*;
    fn input<'a>(root:&'a Path,args:&'a [String],env:&'a BTreeMap<String,String>)->SupervisedChildInput<'a> {
        SupervisedChildInput {run_dir:root,run_id:"run",attempt:1,model:"p/m",thinking:None,next_attempt:None,kind:RunKind::Facts,command:"unused",args:&[],cwd:root,env,hard_deadline_at:chrono::Utc::now().timestamp_millis() as f64+10000.0,termination_grace_ms:5.0,max_output_bytes:1024,supervisor_command:Path::new("/bin/sh"),supervisor_args:args,ledger:serde_json::Map::new()}
    }
    #[tokio::test]
    async fn actual_supervisor_publishes_matching_outcome_and_reaps() {
        let root=tempfile::tempdir().unwrap();
        let args=vec!["-c".into(),"printf 'stdout' > \"$0/child-stdout.log\"; printf 'stderr' > \"$0/child-stderr.log\"; printf '%s' '{\"version\":1,\"runId\":\"run\",\"attempt\":1,\"finishedAt\":\"now\",\"childExit\":{\"code\":0,\"signal\":null},\"timedOut\":false}' > \"$0/outcome.json\"; rm \"$0/launch.json\"".into()];
        let env=BTreeMap::new();
        let result=run_supervised_child(input(root.path(),&args,&env)).await.unwrap();
        assert_eq!(result.code,Some(0));assert_eq!(result.stdout,"stdout");assert_eq!(result.stderr,"stderr");assert!(!root.path().join("launch.json").exists());
        let ledger:serde_json::Value=read_run_json(&root.path().join("ledger.json")).unwrap();assert_eq!(ledger["attempt"],1);assert_eq!(ledger["model"],"p/m");assert_eq!(ledger["launching"],true);
    }
    #[tokio::test]
    async fn supervisor_exit_without_outcome_is_error_and_retains_launch() {
        let root=tempfile::tempdir().unwrap();let args=vec!["-c".into(),"exit 7".into()];let env=BTreeMap::new();
        assert_eq!(run_supervised_child(input(root.path(),&args,&env)).await.unwrap_err(),"memory run supervisor exited with 7");
        assert!(root.path().join("launch.json").exists());
    }
    #[tokio::test]
    async fn wrong_attempt_cannot_authorize_successful_exit() {
        let root=tempfile::tempdir().unwrap();let args=vec!["-c".into(),"printf '%s' '{\"version\":1,\"runId\":\"run\",\"attempt\":2,\"finishedAt\":\"now\",\"childExit\":{\"code\":0,\"signal\":null},\"timedOut\":false}' > \"$0/outcome.json\"; rm \"$0/launch.json\"".into()];let env=BTreeMap::new();
        assert_eq!(run_supervised_child(input(root.path(),&args,&env)).await.unwrap_err(),"memory run supervisor exited with 0");
    }
}
