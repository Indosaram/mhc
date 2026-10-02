use std::{collections::BTreeMap,path::Path};
use memory_core::facts::FactsPayload;
use super::{memory_launch_preflight::{MemoryLaunchPreflightInput,resolve_and_preflight_memory_launch},memory_model_attempts::{MemoryModelAttempt,ReflectionModelCandidate},model_preflight::{ConfigSource,Launcher,ModelPreflight},resolve_model::ReflectionModelResolution,spawn_payload::{PrepareFactsSpawnInput,prepare_facts_spawn},spawn_supervisor::{FactsChildOptions,run_facts_child}};
pub struct FactsChildLaunchInput<'a> {
    pub run_id:&'a str,pub run_dir:&'a Path,pub payload:&'a FactsPayload,pub resolution:&'a ReflectionModelResolution,pub env:&'a BTreeMap<String,String>,pub config_sources:&'a [ConfigSource],pub launch:&'a Launcher,pub hard_deadline_at:f64,pub options:FactsChildOptions<'a>,
}
pub async fn launch_facts_model_chain(cache:&mut ModelPreflight,input:FactsChildLaunchInput<'_>,warn:impl FnOnce(&str))->Result<MemoryModelAttempt,String> {
    let ReflectionModelResolution::Resolved{model,thinking,fallbacks,..}=input.resolution else{return Err("facts child launch requires a resolved model".into());};
    let options=&input.options;
    resolve_and_preflight_memory_launch(cache,MemoryLaunchPreflightInput{first:ReflectionModelCandidate{model:model.clone(),thinking:thinking.clone()},rest:fallbacks,launch:input.launch,env:input.env,env_flag:"SENPI_MEMORY_FACTS",config_sources:input.config_sources,surface_name:"facts",now_ms:options.launched_at},warn,|candidate,attempt,next_attempt|async move {
        let args=prepare_facts_spawn(PrepareFactsSpawnInput{run_id:input.run_id,run_dir:input.run_dir,payload:input.payload,model:&candidate.model,thinking:candidate.thinking.as_deref(),attempt:Some(u32::try_from(attempt).map_err(|error|error.to_string())?),hard_deadline_at:Some(input.hard_deadline_at),next_attempt,env:input.env.clone(),launch:input.launch.clone(),now_ms:options.launched_at as f64}).map_err(|error|error.to_string())?;
        run_facts_child(&args,FactsChildOptions{termination_grace_ms:options.termination_grace_ms,max_output_bytes:options.max_output_bytes,supervisor_command:options.supervisor_command,supervisor_args:options.supervisor_args,batch_id:options.batch_id,queued:options.queued,launched_at:options.launched_at}).await
    }).await
}

#[cfg(all(test,unix))]
mod tests {
    use super::*;
    #[tokio::test]
    async fn native_model_chain_prepares_payload_and_records_matching_supervisor_outcome() {
        let root=tempfile::tempdir().unwrap();let payload=FactsPayload{version:1,identity:"agent".into(),today:"2026-10-02".into(),known_people:vec![],primary_human:memory_core::facts::FactsPrimaryHuman{slug:"human".into(),aliases:vec![]},entries:vec![]};
        let resolution=ReflectionModelResolution::Resolved{category:"quick".into(),model:"p/m".into(),thinking:Some("low".into()),source:None,fallbacks:vec![]};
        let launch=Launcher{command:"/bin/sh".into(),prefix_args:vec!["-c".into(),"printf 'p/m\\n'".into()]};let env=BTreeMap::new();
        let supervisor_args=vec!["-c".into(),"printf '%s' '{\"version\":1,\"runId\":\"run\",\"attempt\":1,\"finishedAt\":\"now\",\"childExit\":{\"code\":0,\"signal\":null},\"timedOut\":false}' > \"$0/outcome.json\"; rm \"$0/launch.json\"".into()];
        let result=launch_facts_model_chain(&mut ModelPreflight::default(),FactsChildLaunchInput{run_id:"run",run_dir:root.path(),payload:&payload,resolution:&resolution,env:&env,config_sources:&[],launch:&launch,hard_deadline_at:chrono::Utc::now().timestamp_millis() as f64+60_000.0,options:FactsChildOptions{termination_grace_ms:100.0,max_output_bytes:1024,supervisor_command:Path::new("/bin/sh"),supervisor_args:&supervisor_args,batch_id:"batch",queued:&[],launched_at:0}},|error|panic!("{error}")).await.unwrap();
        assert_eq!(result.candidate.model,"p/m");assert_eq!(result.child.code,Some(0));
        let ledger:serde_json::Value=super::super::run_artifacts::read_run_json(&root.path().join("ledger.json")).unwrap();assert_eq!(ledger["kind"],"facts");assert_eq!(ledger["batchId"],"batch");assert_eq!(ledger["model"],"p/m");
        let prepared:serde_json::Value=super::super::run_artifacts::read_run_json(&root.path().join("facts-payload.json")).unwrap();assert_eq!(prepared["identity"],"agent");
    }
}
