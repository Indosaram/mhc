use std::{collections::BTreeSet,future::Future};
use memory_core::{identity::resolve::MemoryIdentity,reflection::ReservedRun};
use super::{resolve_model::ReflectionModelResolution,runner_types::{ExecutionResult,ReflectionRunResult,ReflectionReservationPort},runner_execution::ReflectionExecutionResult,completion_delivery::ReflectionLiveSession};
#[derive(Default)]pub struct SenpiSubprocessRunner{warned_categories:BTreeSet<String>}
pub struct ReflectionRunnerInput<'a>{
    pub run:&'a ReservedRun,pub identity:&'a MemoryIdentity,pub config:&'a serde_json::Value,
    pub resolution:&'a ReflectionModelResolution,pub reservation:&'a dyn ReflectionReservationPort,
    pub started_at:&'a str,pub now_ms:&'a dyn Fn()->i64,
}
pub struct NativeReflectionExecutionOptions<'a>{
    pub env:&'a std::collections::BTreeMap<String,String>,pub sources:&'a [super::model_preflight::ConfigSource],pub launcher:&'a super::model_preflight::Launcher,
    pub parent_session_file:Option<&'a std::path::Path>,pub parent_cwd:Option<&'a std::path::Path>,pub route:Option<&'a super::fork_cost::MemoryLaunchRoute>,
    pub sandbox:Option<&'a dyn Fn(super::spawn_types::ReflectionSpawnArgs)->Result<super::spawn_types::ReflectionSpawnArgs,String>>,
    pub deadline_ms:Option<i64>,pub child:super::spawn_supervisor::ReflectionChildOptions<'a>,pub callbacks:NativeReflectionCallbacks<'a>,
}
pub struct NativeReflectionCallbacks<'a>{
    pub ensure_renderer:&'a dyn Fn(),pub health_alert:&'a dyn Fn(&std::path::Path),pub append_launched:&'a dyn Fn()->Result<(),String>,pub warn:&'a dyn Fn(&str),
}
impl SenpiSubprocessRunner{
    pub async fn launch_native(
        &mut self,input:ReflectionRunnerInput<'_>,cache:&mut super::model_preflight::ModelPreflight,options:NativeReflectionExecutionOptions<'_>,
        live:Option<&mut dyn ReflectionLiveSession>,
        terminal_gate:impl FnOnce(&mut dyn FnMut()->Result<Option<super::run_finalization_types::ReservationRunResult>,String>)->Result<Option<super::run_finalization_types::ReservationRunResult>,String>,
    )->Result<ReflectionRunResult,String>{
        let callbacks=options.callbacks;
        let settings=crate::reflection_settings::resolve_agent_reflection_settings(input.config.get("memory"),&input.identity.id)?;
        let deadline=match options.deadline_ms{Some(deadline)=>deadline as f64,None=>settings["timeout_minutes"].as_f64().ok_or("reflection timeout missing")?*60_000.0};
        let merge=settings["merge"].as_str().ok_or("reflection merge policy missing")?;
        let context=super::run_finalization_types::RunFinalizationContext{identity:input.identity,reservation:input.reservation,launch:None,now_ms:input.now_ms};
        let execution=super::runner_execution::ReflectionExecutionInput{run:input.run,resolution:input.resolution,identity:input.identity,config:input.config,env:options.env,sources:options.sources,launch:options.launcher,parent_session_file:options.parent_session_file,parent_cwd:options.parent_cwd,route:options.route,sandbox:options.sandbox,merge_policy:merge,hard_deadline_at:(input.now_ms)() as f64+deadline,options:options.child,finalization:&context};
        let identity=input.identity;
        self.launch(input,live,||super::runner_execution::execute_reflection_run(cache,execution,|repo,run|{
            let exec=memory_core::git::exec::create_git_exec(Default::default());
            memory_core::reflection::create_reflection_worktree(repo,&run.run_id,&identity.paths.worktrees,exec.as_ref(),None).map_err(|error|error.to_string())
        },callbacks.append_launched,callbacks.warn,terminal_gate),callbacks.ensure_renderer,|run|Ok(Self::merged_metadata(identity,run)),callbacks.health_alert).await
    }
    pub fn merged_metadata(identity:&MemoryIdentity,run_id:&str)->(Option<String>,Option<usize>){
        let path=identity.paths.reflection.join("runs").join(run_id).join("ledger.json");
        let Ok(value)=super::run_artifacts::read_run_json(&path)else{return (None,None);};let Ok(ledger)=super::reservation_run_ledger::parse_reservation_run_ledger(value)else{return (None,None);};
        (ledger.value()["integrationSha"].as_str().map(str::to_owned),Some(ledger.value()["validatedChangedPaths"].as_array().map_or(0,Vec::len)))
    }
    pub fn append_launched(run:&ReservedRun,identity:&MemoryIdentity,resolution:&ReflectionModelResolution,started_at:&str,live:Option<&mut dyn ReflectionLiveSession>,mut transcript_state:impl FnMut(&str)->Result<Option<memory_core::journal::cursor::ReflectionTranscriptState>,String>)->Result<(),String>{
        let Some(live)=live else{return Ok(());};let ReflectionModelResolution::Resolved{category,model,thinking,..}=resolution else{return Err("reflection launch entry requires a resolved model".into());};
        let mut backlog_steps=0;for conversation in &run.request.conversation_ids{backlog_steps+=transcript_state(conversation)?.map_or(0,|state|state.steps_since_last_successful_reflection);}
        let entry=super::completion_contracts::ReflectionLaunchedEntry{schema_version:1,run_id:run.run_id.clone(),identity:identity.id.clone(),trigger:run.request.trigger,category:category.clone(),model:Some(model.clone()),thinking:thinking.clone(),conversation_ids:run.request.conversation_ids.clone(),backlog_steps,started_at:started_at.into()};
        live.append_entry(super::completion_contracts::REFLECTION_LAUNCHED_ENTRY_TYPE,serde_json::to_value(entry).map_err(|error|error.to_string())?);Ok(())
    }
    pub fn choose_launch_route(run:&ReservedRun,resolution:&ReflectionModelResolution,registry:Option<&dyn senpi_task::host::SenpiModelRegistry>,session:Option<&super::resolve_model::ReflectionSessionModel>,parent_context_tokens:Option<f64>,cache_hit:bool)->Result<super::fork_cost::MemoryLaunchRoute,String>{
        use super::fork_cost::{MemoryLaunchSurface,MemoryRouteCandidate,MemoryLaunchRouteInput,Pricing,choose_memory_launch_route};
        let ReflectionModelResolution::Resolved{model,thinking,..}=resolution else{return Err("reflection route requires a resolved model".into());};
        let cost=|model:&str|model.split_once('/').and_then(|(provider,id)|registry.and_then(|registry|registry.find(provider,id))).as_ref().and_then(super::registry_fallback::read_model_pricing).map(|cost|Pricing{input:cost.input,cache_read:cost.cache_read,output:None});
        let quick=MemoryRouteCandidate{model:model.clone(),thinking:thinking.clone(),cost:cost(model)};
        let inherited=session.and_then(|session|{let model=format!("{}/{}",session.provider,session.id);cost(&model).map(|cost|MemoryRouteCandidate{model,thinking:session.thinking.clone(),cost:Some(cost)})});
        let surface=if run.request.trigger==memory_core::reflection::ReflectionTrigger::Dream{MemoryLaunchSurface::Dream}else{MemoryLaunchSurface::Reflection};
        choose_memory_launch_route(&MemoryLaunchRouteInput{surface,quick:Some(&quick),session:inherited.as_ref(),parent_context_tokens,turns:surface.profile().turns,cache_hit}).map_err(|error|error.to_string())
    }
    pub async fn launch<F:Future<Output=Result<ReflectionExecutionResult,String>>>(
        &mut self,input:ReflectionRunnerInput<'_>,mut live:Option<&mut dyn ReflectionLiveSession>,
        execute:impl FnOnce()->F,ensure_renderer:impl FnOnce(),
        merged_metadata:impl FnOnce(&str)->Result<(Option<String>,Option<usize>),String>,health_alert:impl FnOnce(&std::path::Path),
    )->Result<ReflectionRunResult,String>{
        if let ReflectionModelResolution::CategoryUnavailable{category,cause,missing_providers,..}=input.resolution{
            if super::resolve_model::should_warn_category_unavailable(input.config,category)&&let Some(session)=live.as_deref_mut()&&self.warned_categories.insert(format!("{}:{category}",session.session_id())){
                let message=match missing_providers.as_ref().filter(|providers|!providers.is_empty()){Some(providers)=>format!("Category \"{category}\" has no usable model: none of its fallback-chain providers are connected ({}).",providers.join(", ")),None=>format!("Category \"{category}\" has no usable model for memory reflection.")};
                super::completion_delivery::safe_notify(session,&message,true);
            }
            let mut detail=format!("Reflection category \"{category}\" could not resolve a usable model (cause: {cause})");
            if let Some(providers)=missing_providers.as_ref().filter(|providers|!providers.is_empty()){detail.push_str(&format!("; missing providers: {}",providers.join(", ")));}
            return super::runner_completion_publication::settle_reflection_run(super::runner_completion_publication::SettleReflectionRunInput{run:input.run,result:ExecutionResult{outcome:"failed".into(),reason:Some("category_unavailable".into()),detail:Some(detail),model:None,thinking:None},started_at:input.started_at,resolution:input.resolution,identity:input.identity,reservation:input.reservation,now_ms:(input.now_ms)(),suppress_completion_notification:true},live,ensure_renderer,health_alert);
        }
        match execute().await?{
            ReflectionExecutionResult::Finalized(result)=>super::runner_completion_publication::publish_finalized_reflection_run(*result,input.identity,(input.now_ms)(),live,ensure_renderer,merged_metadata,health_alert),
            ReflectionExecutionResult::Failed(result)=>super::runner_completion_publication::settle_reflection_run(super::runner_completion_publication::SettleReflectionRunInput{run:input.run,result,started_at:input.started_at,resolution:input.resolution,identity:input.identity,reservation:input.reservation,now_ms:(input.now_ms)(),suppress_completion_notification:false},live,ensure_renderer,health_alert),
        }
    }
}

#[cfg(test)]mod tests{
    use super::*;
    fn reserved()->ReservedRun{ReservedRun{run_id:"run".into(),request:memory_core::reflection::ReflectionRequest{trigger:memory_core::reflection::ReflectionTrigger::Manual,origin:None,conversation_ids:vec![],snapshots:vec![],focus:None,recent_n:None,target_doc:None},reserved_at:None,launcher_pid:None,launcher_hostname:None,launcher_process_start:None}}
    #[test]fn merged_metadata_defaults_only_when_ledger_is_valid(){
        let root=tempfile::tempdir().unwrap();let identity=MemoryIdentity{id:"agent".into(),safe_slug:"agent".into(),paths:memory_core::identity::layout::build_identity_paths(root.path(),"agent")};
        assert_eq!(SenpiSubprocessRunner::merged_metadata(&identity,"run"),(None,None));let dir=identity.paths.reflection.join("runs/run");std::fs::create_dir_all(&dir).unwrap();
        let mut value=serde_json::json!({"version":1,"runId":"run","kind":"reflection","trigger":"manual","startedAt":"now","hardDeadlineAt":100,"terminationGraceMs":5,"deadlineAt":105,"mergePolicy":"auto","worktreeDir":"w","worktreeBranch":"b","baseSha":"sha","gitFilePath":"g","gitFileSnapshot":"snapshot","commonConfigPath":"config","commonConfigSnapshot":null});
        super::super::run_artifacts::write_run_json_atomic(&dir.join("ledger.json"),&value,0o600).unwrap();assert_eq!(SenpiSubprocessRunner::merged_metadata(&identity,"run"),(None,Some(0)));
        value["integrationSha"]="merged".into();value["validatedChangedPaths"]=serde_json::json!(["system/persona.md","notes/fact.md"]);super::super::run_artifacts::write_run_json_atomic(&dir.join("ledger.json"),&value,0o600).unwrap();assert_eq!(SenpiSubprocessRunner::merged_metadata(&identity,"run"),(Some("merged".into()),Some(2)));
    }
    #[test]fn launch_entry_sums_only_available_transcript_backlog(){
        struct Live(Vec<serde_json::Value>);impl ReflectionLiveSession for Live{fn session_id(&self)->&str{"session"}fn append_entry(&mut self,kind:&str,data:serde_json::Value){assert_eq!(kind,super::super::completion_contracts::REFLECTION_LAUNCHED_ENTRY_TYPE);self.0.push(data);}fn notify(&mut self,_:&str,_:bool)->Result<(),String>{panic!("launch does not notify")}fn warn(&mut self,_:&str,_:&str){panic!("warn")}fn on_completion(&mut self,_:&str){panic!("not complete")}}
        let root=tempfile::tempdir().unwrap();let identity=MemoryIdentity{id:"agent".into(),safe_slug:"agent".into(),paths:memory_core::identity::layout::build_identity_paths(root.path(),"agent")};let mut run=reserved();run.request.conversation_ids=vec!["one".into(),"missing".into(),"two".into()];let mut live=Live(vec![]);let resolution=ReflectionModelResolution::Resolved{category:"quick".into(),model:"p/m".into(),thinking:Some("high".into()),source:None,fallbacks:vec![]};
        SenpiSubprocessRunner::append_launched(&run,&identity,&resolution,"now",Some(&mut live),|conversation|{if conversation=="missing"{return Ok(None);}let mut state=memory_core::journal::cursor::initial_reflection_state();state.steps_since_last_successful_reflection=if conversation=="one"{3}else{5};Ok(Some(state))}).unwrap();
        assert_eq!(live.0.len(),1);assert_eq!(live.0[0]["backlogSteps"],8);assert_eq!(live.0[0]["model"],"p/m");assert_eq!(live.0[0]["thinking"],"high");assert_eq!(live.0[0]["conversationIds"],serde_json::json!(["one","missing","two"]));
    }
    #[test]fn launch_route_uses_registry_pricing_and_measured_workload(){
        struct Registry;impl senpi_task::host::SenpiModelRegistry for Registry{fn get_available(&self)->Result<serde_json::Value,senpi_task::host::HostError>{panic!("route only reads catalog")}fn find(&self,provider:&str,id:&str)->Option<serde_json::Value>{Some(serde_json::json!({"provider":provider,"id":id,"cost":{"input":if id=="session"{0.001}else{100.0},"cacheRead":0.0001}}))}}
        let run=reserved();let resolution=ReflectionModelResolution::Resolved{category:"quick".into(),model:"p/quick".into(),thinking:None,source:None,fallbacks:vec![]};let session=super::super::resolve_model::ReflectionSessionModel{provider:"p".into(),id:"session".into(),thinking:Some("low".into())};
        let route=SenpiSubprocessRunner::choose_launch_route(&run,&resolution,Some(&Registry),Some(&session),Some(1000.0),true).unwrap();assert_eq!(route.route,super::super::fork_cost::Route::Fork);assert_eq!(route.model,"p/session");assert_eq!(route.thinking.as_deref(),Some("low"));
        let route=SenpiSubprocessRunner::choose_launch_route(&run,&resolution,Some(&Registry),Some(&session),None,true).unwrap();assert_eq!(route.route,super::super::fork_cost::Route::Quick);assert_eq!(route.model,"p/quick");
    }
    struct Reservation;
    #[tokio::test]async fn native_unavailable_category_does_not_initialize_engine_or_preflight(){
        let root=tempfile::tempdir().unwrap();let identity=MemoryIdentity{id:"agent".into(),safe_slug:"agent".into(),paths:memory_core::identity::layout::build_identity_paths(root.path(),"agent")};let run=reserved();
        let resolution=ReflectionModelResolution::CategoryUnavailable{category:"quick".into(),cause:"no_registry",attempted_chain:None,missing_providers:None};let config=serde_json::json!({});let env=Default::default();let launcher=super::super::model_preflight::Launcher{command:"missing-must-not-run".into(),prefix_args:vec![]};
        let result=SenpiSubprocessRunner::default().launch_native(ReflectionRunnerInput{run:&run,identity:&identity,config:&config,resolution:&resolution,reservation:&Reservation,started_at:"1970-01-01T00:00:00.000Z",now_ms:&||0},&mut super::super::model_preflight::ModelPreflight::default(),NativeReflectionExecutionOptions{env:&env,sources:&[],launcher:&launcher,parent_session_file:None,parent_cwd:None,route:None,sandbox:None,deadline_ms:None,child:super::super::spawn_supervisor::ReflectionChildOptions{termination_grace_ms:5000.0,max_output_bytes:1024,supervisor_command:std::path::Path::new("missing-supervisor"),supervisor_args:&[],launched_at:0},callbacks:NativeReflectionCallbacks{ensure_renderer:&||{},health_alert:&|_|{},append_launched:&||panic!("no launch"),warn:&|_|panic!("no preflight")}},None,|_|panic!("no terminal gate")).await.unwrap();
        assert_eq!(result.reason.as_deref(),Some("category_unavailable"));assert!(!identity.paths.repo.exists());
    }
    #[tokio::test]async fn completion_samples_clock_after_execution(){
        let root=tempfile::tempdir().unwrap();let identity=MemoryIdentity{id:"agent".into(),safe_slug:"agent".into(),paths:memory_core::identity::layout::build_identity_paths(root.path(),"agent")};
        let run=ReservedRun{run_id:"run".into(),request:memory_core::reflection::ReflectionRequest{trigger:memory_core::reflection::ReflectionTrigger::Manual,origin:None,conversation_ids:vec![],snapshots:vec![],focus:None,recent_n:None,target_doc:None},reserved_at:None,launcher_pid:None,launcher_hostname:None,launcher_process_start:None};
        let resolution=ReflectionModelResolution::Resolved{category:"quick".into(),model:"p/m".into(),thinking:None,source:None,fallbacks:vec![]};let config=serde_json::json!({});let now=std::cell::Cell::new(0);
        let result=SenpiSubprocessRunner::default().launch(ReflectionRunnerInput{run:&run,identity:&identity,config:&config,resolution:&resolution,reservation:&Reservation,started_at:"1970-01-01T00:00:00.000Z",now_ms:&||now.get()},None,||async{now.set(2500);Ok(ReflectionExecutionResult::Failed(ExecutionResult{outcome:"failed".into(),reason:Some("spawn_failed".into()),detail:None,model:None,thinking:None}))},||{},|_|panic!("not merged"),|_|{}).await.unwrap();
        assert_eq!(result.completion.duration_ms,Some(2500.0));assert_eq!(result.completion.finished_at,"1970-01-01T00:00:02.500Z");
    }
    impl ReflectionReservationPort for Reservation{
        fn read_state(&self)->Result<memory_core::reflection::ReservationState,String>{Ok(Default::default())}
        fn complete(&self,_:&str,outcome:memory_core::reflection::ReflectionOutcome)->Result<memory_core::reflection::CompletionResult,String>{Ok(memory_core::reflection::CompletionResult{outcome,launch:None})}
    }
    #[tokio::test]async fn unavailable_category_settles_without_launching_child(){let root=tempfile::tempdir().unwrap();let identity=MemoryIdentity{id:"agent".into(),safe_slug:"agent".into(),paths:memory_core::identity::layout::build_identity_paths(root.path(),"agent")};let run=ReservedRun{run_id:"run".into(),request:memory_core::reflection::ReflectionRequest{trigger:memory_core::reflection::ReflectionTrigger::Manual,origin:None,conversation_ids:vec![],snapshots:vec![],focus:None,recent_n:None,target_doc:None},reserved_at:None,launcher_pid:None,launcher_hostname:None,launcher_process_start:None};let resolution=ReflectionModelResolution::CategoryUnavailable{category:"quick".into(),cause:"no_registry",attempted_chain:None,missing_providers:Some(vec!["provider".into()])};let config=serde_json::json!({});let result=SenpiSubprocessRunner::default().launch(ReflectionRunnerInput{run:&run,identity:&identity,config:&config,resolution:&resolution,reservation:&Reservation,started_at:"1970-01-01T00:00:00.000Z",now_ms:&||1000},None,||async{panic!("unavailable category must not execute")},||{},|_|panic!("not merged"),|_|{}).await.unwrap();assert_eq!(result.reason.as_deref(),Some("category_unavailable"));assert!(result.completion.model.is_none());assert!(identity.paths.reflection.join("completions/run.json").exists());}
}
