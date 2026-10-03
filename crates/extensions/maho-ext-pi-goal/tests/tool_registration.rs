use maho_ext_api::*;
use maho_ext_pi_goal::goal::{tool_registration::*,types::*,store::*};
use std::{path::{Path,PathBuf},sync::{Arc,Mutex}};
struct Session;
impl ToolSessionManager for Session{fn session_id(&self)->&str{"fixture"}fn session_file(&self)->Option<&Path>{None}}
impl SessionManager for Session{fn get_entries(&self)->Vec<SessionEntry>{Vec::new()}fn get_branch(&self)->Vec<SessionEntry>{Vec::new()}fn get_leaf_id(&self)->Option<String>{None}fn get_session_name(&self)->Option<String>{None}fn extension_context_actions(&self)->Option<&dyn ExtensionContextActions>{Some(self)}}
impl ExtensionContextActions for Session{
    fn get_model(&self)->Option<Model>{None}
    fn get_service_tier(&self)->Option<ServiceTier>{None}
    fn get_scoped_models(&self)->Vec<ScopedModel>{Vec::new()}
    fn get_agent_dir(&self)->PathBuf{"/fixture/agent".into()}
    fn is_idle(&self)->bool{true}
    fn is_project_trusted(&self)->bool{true}
    fn get_signal(&self)->Option<AbortSignal>{None}
    fn abort(&self,_:Option<AbortSource>){panic!("unexpected abort")}
    fn has_pending_messages(&self)->bool{false}
    fn request_reload(&self)->ExtensionFuture<'_,()>{Box::pin(async{Err("unexpected reload".into())})}
    fn is_compacting(&self)->bool{false}
    fn check_reload_veto(&self)->ExtensionFuture<'_,ReloadVetoDecision>{Box::pin(async{Ok(Default::default())})}
    fn shutdown(&self){panic!("unexpected shutdown")}
    fn get_context_usage(&self)->Option<ContextUsage>{None}
    fn get_compaction_settings(&self)->CompactionSettings{panic!("unexpected settings")}
    fn get_prompt_cache_safe_wait_seconds(&self)->Option<f64>{None}
    fn get_prompt_cache_goal_backstop_max_seconds(&self)->f64{0.0}
    fn get_prompt_cache_keep_alive_settings(&self)->PromptCacheKeepAliveSettings{panic!("unexpected settings")}
    fn get_look_at_settings(&self)->LookAtSettings{panic!("unexpected settings")}
    fn get_ask_user_settings(&self)->AskUserSettings{panic!("unexpected settings")}
    fn get_image_settings(&self)->ImageSettings{panic!("unexpected settings")}
    fn session_settings(&self)->&dyn ExtensionSessionSettings{panic!("unexpected settings")}
    fn compact(&self,_:CompactOptions){panic!("unexpected compaction")}
    fn prepare_provider_request(&self,_:Vec<AgentMessage>)->ExtensionFuture<'_,ProviderRequestPreparation>{Box::pin(async{Err("unexpected provider request".into())})}
    fn begin_compaction(&self,_:BeginCompactionOptions)->Option<AbortSignal>{panic!("unexpected compaction")}
    fn update_compaction(&self,_:UpdateCompactionOptions){panic!("unexpected compaction")}
    fn end_compaction(&self,_:EndCompactionOptions){panic!("unexpected compaction")}
    fn get_message_revision(&self)->u64{0}
    fn apply_compaction(&self,_:CompactionResult,_:ApplyCompactionOptions)->ExtensionFuture<'_,ApplyCompactionResult>{Box::pin(async{Err("unexpected compaction".into())})}
    fn get_system_prompt(&self)->String{String::new()}
    fn get_system_prompt_options(&self)->BuildSystemPromptOptions{Default::default()}
    fn get_loaded_hook_sources(&self)->LoadedHookSources{panic!("unexpected hooks")}
    fn kernel_tools(&self)->Option<&dyn ExtensionKernelTools>{None}
}
struct Registry;
impl ModelRegistry for Registry{fn get_all(&self)->Vec<Model>{Vec::new()}fn get_available(&self)->Vec<Model>{Vec::new()}fn find(&self,_:&str,_:&str)->Option<Model>{None}fn has_configured_auth(&self,_:&Model)->bool{false}fn get_api_key_for_provider<'a>(&'a self,_:&'a str)->ExtensionFuture<'a,Option<String>>{Box::pin(async{Ok(None)})}}
#[derive(Default)]
struct Ui { confirmation: bool, statuses: Mutex<Vec<Option<String>>> }
impl ExtensionUi for Ui{
    fn select<'a>(&'a self,_:&'a str,options:&'a [String],_:ExtensionUiDialogOptions)->UiFuture<'a,Option<String>>{Box::pin(async move {if self.confirmation { options.first().cloned() } else { None }})}
    fn confirm<'a>(&'a self,_:&'a str,_:&'a str,_:ExtensionUiDialogOptions)->UiFuture<'a,bool>{Box::pin(async move {self.confirmation})}
    fn input<'a>(&'a self,_:&'a str,_:Option<&'a str>,_:ExtensionUiDialogOptions)->UiFuture<'a,Option<String>>{Box::pin(async{None})}
    fn notify(&self,_:&str,_:NotificationType){}fn set_status(&self,_:&str,text:Option<&str>){self.statuses.lock().expect("status receipt").push(text.map(str::to_owned));}fn set_widget(&self,_:&str,_:Option<WidgetContent>,_:ExtensionWidgetOptions){}fn set_header(&self,_:Option<ComponentFactory>){}fn set_footer(&self,_:Option<ComponentFactory>){}fn set_title(&self,_:&str){}fn paste_to_editor(&self,_:&str){}fn set_editor_text(&self,_:&str){}fn get_editor_text(&self)->String{String::new()}
    fn custom(&self,_:ComponentFactory,_:CustomUiOptions)->ExtensionFuture<'_,JsonValue>{Box::pin(async{Err("headless fixture".into())})}fn theme(&self)->Theme{Theme::default()}
}
#[tokio::test]
async fn registered_command_persists_pause_resume_replace_and_clear(){
    use maho_ext_pi_goal::goal::lifecycle::{GoalLifecycle,RegisteredGoalLifecycle};
    let temp=tempfile::tempdir().expect("fixture");let ctx=context(temp.path());
    let path=temp.path().join("commands");let resolved=path.clone();
    let reference=GoalStoreRef{base_dir:path,thread_id:"fixture".into()};
    let continuations=Arc::new(Mutex::new(Vec::new()));let sent=Arc::clone(&continuations);
    let deps=Arc::new(RegisteredGoalLifecycle{state:Arc::new(Mutex::new(GoalLifecycle::default())),resolve:Arc::new(move|_|GoalStoreRef{base_dir:resolved.clone(),thread_id:"fixture".into()}),send:Arc::new(move|prompt|{sent.lock().expect("continuations").push(prompt);Ok(())})});
    let mut api=ExtensionApi::new(LoadedExtension::new("pi-goal",temp.path().into(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),ExtensionRuntime::default());
    maho_ext_pi_goal::goal::command_registration::register_goal_command(&mut api,deps);
    let command=&api.registered.commands[0].handler;
    command("",&ctx).await.expect("empty show");
    command("first objective",&ctx).await.expect("set");
    let first=read_goal(&reference).expect("store").expect("goal");
    command("pause",&ctx).await.expect("pause");assert_eq!(read_goal(&reference).expect("store").expect("goal").status,GoalStatus::Paused);
    command("resume",&ctx).await.expect("resume");assert_eq!(read_goal(&reference).expect("store").expect("goal").id,first.id);
    command("second objective",&ctx).await.expect("replace");assert_ne!(read_goal(&reference).expect("store").expect("goal").id,first.id);
    assert_eq!(continuations.lock().expect("continuations").len(),3);
    command("clear",&ctx).await.expect("clear");assert_eq!(read_goal(&reference).expect("store"),None);
    command("clear",&ctx).await.expect("clear empty");
}
fn context(cwd:&Path)->ExtensionContext{ExtensionContext{ui:Arc::new(Ui::default()),mode:ExtensionMode::Print,has_ui:false,cwd:cwd.into(),agent_dir:cwd.join("agent"),session_manager:Arc::new(Session),model_registry:Arc::new(Registry),model:None,thinking_level:None,service_tier:None,effective_service_tier:None,scoped_models:Vec::new(),goal_store_file:None,loaded_extension_paths:Vec::new(),signal:None,steering_signal:None,is_idle_fn:Arc::new(||true),wait_for_idle_fn:Arc::new(||Box::pin(async{})),is_project_trusted_fn:Arc::new(||true),is_compacting_fn:Arc::new(||false),get_system_prompt_fn:Arc::new(String::new),get_system_prompt_options_fn:Arc::new(BuildSystemPromptOptions::default),registered_mcp_servers:Vec::new(),update_tool_hook_status:None}}

#[tokio::test]
async fn registered_goal_confirmation_cancellation_and_ui_are_session_isolated() {
    let temp = tempfile::tempdir().expect("fixture");
    let resolve: maho_ext_pi_goal::goal::lifecycle::GoalStoreResolver = Arc::new(|ctx| GoalStoreRef { base_dir: ctx.cwd.join("goals"), thread_id: "fixture".into() });
    let mut api = ExtensionApi::new(LoadedExtension::new("pi-goal", temp.path().into(), Default::default()), Default::default(), Default::default(), Default::default());
    maho_ext_pi_goal::index::register_goal_extension(&mut api, Arc::clone(&resolve), Arc::new(|_| Ok(()))).expect("register");
    let command = &api.registered.commands[0].handler;
    let cancelled_ui = Arc::new(Ui::default());
    let confirmed_ui = Arc::new(Ui { confirmation: true, ..Default::default() });
    let mut first = context(&temp.path().join("first"));
    first.has_ui = true;
    first.ui = cancelled_ui.clone();
    let mut second = context(&temp.path().join("second"));
    second.has_ui = true;
    second.ui = confirmed_ui.clone();
    command("first objective", &first).await.expect("first goal");
    command("second objective", &second).await.expect("second goal");
    let original = read_goal(&resolve(&first)).expect("read").expect("first");
    command("replacement cancelled", &first).await.expect("cancelled replacement");
    command("replacement confirmed", &second).await.expect("confirmed replacement");
    assert_eq!(read_goal(&resolve(&first)).expect("read").expect("first"), original);
    assert_eq!(read_goal(&resolve(&second)).expect("read").expect("second").objective, "replacement confirmed");
    assert_eq!(cancelled_ui.statuses.lock().expect("statuses").len(), 1);
    command("pause", &second).await.expect("pause");
    command("resume", &second).await.expect("resume");
    command("clear", &second).await.expect("clear");
    assert!(read_goal(&resolve(&second)).expect("read").is_none());
    let statuses = confirmed_ui.statuses.lock().expect("statuses");
    assert!(statuses.iter().any(Option::is_some));
    assert_eq!(statuses.last(), Some(&None));
}
struct Deps{path:PathBuf,events:Mutex<Vec<&'static str>>}
impl GoalToolRegistrationDeps for Deps{
    fn goal_store_ref(&self,_:&ExtensionContext)->GoalStoreRef{GoalStoreRef{base_dir:self.path.clone(),thread_id:"fixture".into()}}
    fn begin_agent_goal_accounting(&self,_:&Goal){self.events.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push("begin");}
    fn mark_goal_blocked_this_turn(&self,_:&Goal){self.events.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push("blocked");}
    fn mark_goal_completed_this_turn(&self,_:&Goal){self.events.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push("complete");}
    fn account_current_agent_turn<'a>(&'a self,ctx:&'a ExtensionContext,_:GoalAccountingMode)->ExtensionFuture<'a,Option<Goal>>{Box::pin(async move{self.events.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push("account");read_goal(&self.goal_store_ref(ctx)).map_err(ExtensionFailure::new)})}
}
#[tokio::test]
async fn tools_enforce_status_contract_and_account_before_update(){
    let temp=tempfile::tempdir().expect("temp fixture");let ctx=context(temp.path());let deps=Deps{path:temp.path().join("goals"),events:Mutex::new(Vec::new())};let reference=deps.goal_store_ref(&ctx);
    create_goal_at(&reference,"objective",10,"fixed-id".into()).expect("initial goal");
    assert!(execute_create_goal("replacement",&ctx,&deps).await.is_err());assert!(execute_update_goal(ModelSettableGoalStatus::Blocked,Some("  "),&ctx,&deps).await.is_err());assert!(execute_update_goal(ModelSettableGoalStatus::Complete,Some("reason"),&ctx,&deps).await.is_err());assert!(deps.events.lock().expect("events").is_empty());
    let result=execute_update_goal(ModelSettableGoalStatus::Blocked,Some(" needs input "),&ctx,&deps).await.expect("block");assert_eq!(result.details,Some(serde_json::json!({})));let goal=read_goal(&reference).expect("read").expect("goal");assert_eq!(goal.status,GoalStatus::Blocked);assert_eq!(goal.blocked_reason.as_deref(),Some("needs input"));assert_eq!(*deps.events.lock().expect("events"),vec!["account","blocked"]);
    execute_get_goal(&ctx,&deps).await.expect("get");execute_update_goal(ModelSettableGoalStatus::Complete,None,&ctx,&deps).await.expect("complete");assert_eq!(read_goal(&reference).expect("read").expect("goal").status,GoalStatus::Complete);
}

#[tokio::test]
async fn registered_goal_tools_execute_with_live_context(){
    let temp=tempfile::tempdir().expect("fixture");let ctx=context(temp.path());
    let deps=Arc::new(Deps{path:temp.path().join("registered"),events:Mutex::new(Vec::new())});
    let runtime=ExtensionRuntime::default();
    let mut api=ExtensionApi::new(LoadedExtension::new("pi-goal",temp.path().into(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),runtime.clone());
    register_goal_tools(&mut api,deps.clone()).expect("registration");
    assert_eq!(api.registered.tools.len(),3);
    let create=runtime.extension_tool_executor("pi-goal","create_goal").expect("live create");
    let result=create("create",serde_json::json!({"objective":"registered objective"}),None,None,&ctx).await.expect("create");
    assert_eq!(result.details,serde_json::json!({}));
    assert!(create("duplicate",serde_json::json!({"objective":"replacement"}),None,None,&ctx).await.is_err());
    let update=runtime.extension_tool_executor("pi-goal","update_goal").expect("live update");
    assert!(update("invalid",serde_json::json!({"status":"paused"}),None,None,&ctx).await.is_err());
    update("complete",serde_json::json!({"status":"complete"}),None,None,&ctx).await.expect("complete");
    let get=runtime.extension_tool_executor("pi-goal","get_goal").expect("live get");
    let result=get("get",serde_json::json!({}),None,None,&ctx).await.expect("get");
    assert_eq!(result.details,serde_json::json!({}));
    assert_eq!(read_goal(&deps.goal_store_ref(&ctx)).expect("store").expect("goal").status,GoalStatus::Complete);
    assert_eq!(*deps.events.lock().expect("events"),["begin","account","complete","account"]);
}
#[tokio::test]
async fn create_replaces_completed_goal_and_preserves_full_objective(){
    let temp=tempfile::tempdir().expect("temp fixture");let ctx=context(temp.path());let deps=Deps{path:temp.path().join("goals"),events:Mutex::new(Vec::new())};let reference=deps.goal_store_ref(&ctx);
    create_goal_at(&reference,"previous",10,"fixed-id".into()).expect("initial goal");update_goal_at(&reference,&GoalUpdate{status:Some(GoalStatus::Complete),..Default::default()},GoalUpdateSource::Model,11,"unused".into()).expect("complete");
    let objective="x".repeat(4100);let result=execute_create_goal(&objective,&ctx,&deps).await.expect("create");assert_eq!(result.details,Some(serde_json::json!({})));let goal=read_goal(&reference).expect("read").expect("goal");assert_eq!(goal.status,GoalStatus::Active);assert_eq!(goal.objective.chars().count(),4000);assert_eq!(std::fs::read_to_string(objective_full_text_file_path(&reference)).expect("full objective"),objective);assert!(goal_history_file_path(&reference).exists());assert_eq!(*deps.events.lock().expect("events"),vec!["begin"]);
}
#[tokio::test]
async fn registered_lifecycle_marks_live_tool_updates(){
    use maho_ext_pi_goal::goal::lifecycle::{GoalLifecycle,RegisteredGoalLifecycle};
    let temp=tempfile::tempdir().expect("temp");let ctx=context(temp.path());let path=temp.path().join("goals");let resolved=path.clone();let state=Arc::new(Mutex::new(GoalLifecycle::default()));state.lock().expect("state").agent_turn_in_progress=true;let deps=RegisteredGoalLifecycle{state:Arc::clone(&state),resolve:Arc::new(move|_|GoalStoreRef{base_dir:resolved.clone(),thread_id:"fixture".into()}),send:Arc::new(|_|Ok(()))};let reference=GoalStoreRef{base_dir:path,thread_id:"fixture".into()};create_goal_at(&reference,"objective",10,"fixed-id".into()).expect("goal");execute_update_goal(ModelSettableGoalStatus::Blocked,Some("input needed"),&ctx,&deps).await.expect("block");assert_eq!(state.lock().expect("state").blocked_this_turn_goal_id.as_deref(),Some("fixed-id"));execute_update_goal(ModelSettableGoalStatus::Complete,None,&ctx,&deps).await.expect("complete");let state=state.lock().expect("state");assert_eq!(state.completed_this_turn_goal_id.as_deref(),Some("fixed-id"));assert_eq!(state.agent_goal_accounting.as_ref().map(|(id,_)|id.as_str()),Some("fixed-id"));
}
