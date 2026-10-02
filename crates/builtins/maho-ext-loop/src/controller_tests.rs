use std::{sync::{Arc,Mutex},path::Path};
use maho_ext_api::*;
use crate::{extension::LoopExtension,index::{LoopController,StartDynamicRequest,LoopCreateOutcome},tools::ScheduleWakeupSchedulerPort};
struct Session;
impl ToolSessionManager for Session { fn session_id(&self)->&str { "s" } fn session_file(&self)->Option<&Path> { None } }
impl SessionManager for Session {
    fn get_entries(&self)->Vec<SessionEntry> { Vec::new() }
    fn get_branch(&self)->Vec<SessionEntry> { Vec::new() }
    fn get_leaf_id(&self)->Option<String> { None }
    fn get_session_name(&self)->Option<String> { None }
}
struct Registry;
impl ModelRegistry for Registry {
    fn get_all(&self)->Vec<Model> { Vec::new() }
    fn get_available(&self)->Vec<Model> { Vec::new() }
    fn find(&self,_:&str,_:&str)->Option<Model> { None }
    fn has_configured_auth(&self,_:&Model)->bool { false }
    fn get_api_key_for_provider<'a>(&'a self,_:&'a str)->ExtensionFuture<'a,Option<String>> { Box::pin(async { Ok(None) }) }
}
struct Ui;
impl ExtensionUi for Ui {
    fn select<'a>(&'a self,_:&'a str,_:&'a [String],_:ExtensionUiDialogOptions)->UiFuture<'a,Option<String>> { Box::pin(async { None }) }
    fn confirm<'a>(&'a self,_:&'a str,_:&'a str,_:ExtensionUiDialogOptions)->UiFuture<'a,bool> { Box::pin(async { false }) }
    fn input<'a>(&'a self,_:&'a str,_:Option<&'a str>,_:ExtensionUiDialogOptions)->UiFuture<'a,Option<String>> { Box::pin(async { None }) }
    fn notify(&self,_:&str,_:NotificationType) {}
    fn set_status(&self,_:&str,_:Option<&str>) {}
    fn set_widget(&self,_:&str,_:Option<WidgetContent>,_:ExtensionWidgetOptions) {}
    fn set_header(&self,_:Option<ComponentFactory>) {}
    fn set_footer(&self,_:Option<ComponentFactory>) {}
    fn set_title(&self,_:&str) {}
    fn paste_to_editor(&self,_:&str) {}
    fn set_editor_text(&self,_:&str) {}
    fn get_editor_text(&self)->String { String::new() }
    fn custom(&self,_:ComponentFactory,_:CustomUiOptions)->ExtensionFuture<'_,JsonValue> { Box::pin(async { Err("UI unavailable".into()) }) }
    fn theme(&self)->Theme { Theme::default() }
}
fn context()->ExtensionContext {
    ExtensionContext { ui:Arc::new(Ui),mode:ExtensionMode::Tui,has_ui:true,cwd:"/tmp".into(),agent_dir:"/tmp/agent".into(),session_manager:Arc::new(Session),model_registry:Arc::new(Registry),model:None,thinking_level:None,service_tier:None,effective_service_tier:None,scoped_models:Vec::new(),goal_store_file:None,loaded_extension_paths:Vec::new(),signal:None,steering_signal:None,is_idle_fn:Arc::new(||true),wait_for_idle_fn:Arc::new(||Box::pin(async {})),is_project_trusted_fn:Arc::new(||true),is_compacting_fn:Arc::new(||false),get_system_prompt_fn:Arc::new(String::new),get_system_prompt_options_fn:Arc::new(BuildSystemPromptOptions::default),registered_mcp_servers:Vec::new(),update_tool_hook_status:None }
}
fn real_session()->maho_core::agent_session::AgentSession {
    use maho_core::{agent_session::*,settings_manager::*,session_manager::SessionManager,model_runtime::*};
    let stream:maho_agent::types::StreamFn=Arc::new(|_,_,_|maho_ai::types::AssistantMessageEventStream::assistant());
    AgentSession::new(AgentSessionConfig { agent:maho_agent::Agent::new(maho_agent::AgentOptions { stream_fn:Some(stream),..Default::default() }),session_manager:SessionManager::in_memory("/tmp",None,None),settings_manager:SettingsManager::from_storage(Box::<InMemorySettingsStorage>::default(),false),cwd:"/tmp".into(),agent_dir:Some("/tmp/maho-agent".into()),fallback_now:None,retry_random:None,scoped_models:Vec::new(),favorite_models:Vec::new(),flag_values:Default::default(),custom_tools:Vec::new(),model_runtime:Some(ModelRuntime::create_sync(CreateModelRuntimeOptions { providers:Some(Vec::new()),..Default::default() })),model_registry:None,uses_default_stream_function:Some(false),initial_active_tool_names:None,default_tool_names:None,eval_only_tool_names:None,allowed_tool_names:None,excluded_tool_names:None,base_tools_override:None,session_start_event:None,auto_title_sessions:Some(false) }).unwrap()
}
#[tokio::test] async fn registered_session_creation_dispatch_wakeup_pause_and_shutdown() {
    let dir=tempfile::tempdir().unwrap(); let base=dir.path().to_path_buf();
    let ready=Arc::new(Mutex::new(None)); let capture=ready.clone();
    let mut extension=LoopExtension::new(Arc::new(move |ctx|crate::types::LoopStoreRef { base_dir:base.clone(),session_id:ctx.session_manager.session_id().into() }));
    extension.now=Arc::new(||1000.0); extension.on_controller_ready=Some(Arc::new(move |controller|*capture.lock().unwrap()=Some(controller)));
    let session=real_session(); let runner=maho_ext_host::ExtensionRunner::from_static(vec![Box::new(extension)],context());
    session.set_extension_runner(runner).await;
    session.bind_extensions(Default::default()).await;
    let controller=ready.lock().unwrap().clone().unwrap();
    let LoopCreateOutcome::Created(created)=controller.start_dynamic(StartDynamicRequest { original_args:"check".into(),prompt:"check".into() }).await.unwrap() else { panic!("creation rejected") };
    let entries=session.with_session_manager(|manager|manager.entries().to_vec());
    assert!(entries.iter().any(|entry|entry["customType"]=="loop-tick"&&entry["data"]["loopId"]==created.loop_id));
    assert!(entries.iter().find(|entry|entry["customType"]=="loop-tick").unwrap()["data"]["deliveryId"].as_str().unwrap().starts_with("delivery-"));
    let target=controller.get_wakeup_target().unwrap(); assert_eq!(target.loop_id,created.loop_id);
    crate::tools::ScheduleWakeupSchedulerPort::schedule_wakeup(controller.as_ref(),crate::tools::ScheduleWakeupRequest { loop_id:created.loop_id.clone(),requested_delay_seconds:60.0,delay_seconds:60.0,reason:"wait".into(),prompt:"check".into(),noop:false }).await.unwrap();
    controller.event(&ExtensionEvent::AgentEnd { messages:Vec::new(),aborted:Some(false),abort_source:None,will_retry:Some(false) }).await.unwrap();
    assert!(controller.get_wakeup_target().is_none());
    assert_eq!(controller.pause(&created.loop_id).await.unwrap(),vec![created.loop_id.clone()]);
    assert_eq!(controller.resume(&created.loop_id).await.unwrap(),vec![created.loop_id.clone()]);
    controller.event(&ExtensionEvent::SessionShutdown(SessionShutdownEvent { reason:SessionReason::Quit,target_session_file:None,signal:None })).await.unwrap();
    let saved=crate::store::read_loop_state(&crate::types::LoopStoreRef { base_dir:dir.path().into(),session_id:"s".into() }).await.unwrap().unwrap();
    let crate::types::CronEntry::Dynamic { lifecycle,.. }=&saved.entries[&created.loop_id] else { panic!("expected dynamic") };
    assert_eq!(lifecycle.phase,crate::types::LoopPhase::Suspended); assert!(lifecycle.end_reason.is_none());
}
#[tokio::test] async fn registered_command_and_tool_share_live_runtime_attribution() {
    let dir=tempfile::tempdir().unwrap(); let base=dir.path().to_path_buf();
    let ready=Arc::new(Mutex::new(None)); let capture=ready.clone();
    let mut extension=LoopExtension::new(Arc::new(move |ctx|crate::types::LoopStoreRef { base_dir:base.clone(),session_id:ctx.session_manager.session_id().into() }));
    extension.now=Arc::new(||1000.0); extension.on_controller_ready=Some(Arc::new(move |controller|*capture.lock().unwrap()=Some(controller)));
    let runtime=ExtensionRuntime::default(); let events=EventBus::default();
    let mut api=ExtensionApi::new(LoadedExtension::new("loop","/tmp".into(),SourceInfo::default()),Default::default(),events.clone(),runtime.clone()); extension.register(&mut api);
    let command=api.registered.commands.iter().find(|command|command.name=="loop").unwrap().handler.clone();
    let tool=api.registered.tools.iter().find(|tool|tool.definition.name=="schedule_wakeup").unwrap().definition.execute.clone();
    let session=real_session(); let runner=maho_ext_host::ExtensionRunner::new(vec![api.registered],runtime,events,context());
    session.set_extension_runner(runner).await; session.bind_extensions(Default::default()).await;
    command("check",&context()).await.unwrap();
    let controller=ready.lock().unwrap().clone().unwrap(); let target=controller.get_wakeup_target().unwrap();
    let result=tool(ToolCall { id:"wake",params:serde_json::json!({"delaySeconds":1,"reason":"wait","prompt":"/loop check"}),signal:AbortSignal::default(),on_update:None,context:None }).await.unwrap();
    assert_eq!(result.details.as_ref().unwrap()["delaySeconds"],60.0); assert_eq!(result.details.as_ref().unwrap()["loopId"],target.loop_id);
    command(&format!("pause {}",target.loop_id),&context()).await.unwrap();
    let saved=crate::store::read_loop_state(&crate::types::LoopStoreRef { base_dir:dir.path().into(),session_id:"s".into() }).await.unwrap().unwrap();
    let crate::types::CronEntry::Dynamic { lifecycle,pending_wakeup,.. }=&saved.entries[&target.loop_id] else { panic!("expected dynamic") };
    assert_eq!(lifecycle.phase,crate::types::LoopPhase::Suspended); assert_eq!(pending_wakeup.as_ref().unwrap().prompt,"/loop check");
    command("stop all",&context()).await.unwrap(); assert!(controller.get_wakeup_target().is_none());
    controller.event(&ExtensionEvent::SessionShutdown(SessionShutdownEvent { reason:SessionReason::Quit,target_session_file:None,signal:None })).await.unwrap();
}
#[tokio::test] async fn unreadable_store_fails_closed_without_dispatch() {
    let dir=tempfile::tempdir().unwrap(); let base=dir.path().join("file"); std::fs::write(&base,"not a directory").unwrap();
    let ready=Arc::new(Mutex::new(None)); let capture=ready.clone();
    let mut extension=LoopExtension::new(Arc::new(move |ctx|crate::types::LoopStoreRef { base_dir:base.clone(),session_id:ctx.session_manager.session_id().into() }));
    extension.on_controller_ready=Some(Arc::new(move |controller|*capture.lock().unwrap()=Some(controller)));
    let session=real_session(); let runner=maho_ext_host::ExtensionRunner::from_static(vec![Box::new(extension)],context());
    session.set_extension_runner(runner).await; session.bind_extensions(Default::default()).await;
    let controller=ready.lock().unwrap().clone().unwrap();
    assert!(controller.last_store_failure().is_some());
    let LoopCreateOutcome::Created(created)=controller.start_dynamic(StartDynamicRequest { original_args:"check".into(),prompt:"check".into() }).await.unwrap() else { panic!("creation rejected") };
    assert!(controller.is_ended_with_error(&created.loop_id));
    assert!(controller.get_wakeup_target().is_none());
    assert!(!session.with_session_manager(|manager|manager.entries().iter().any(|entry|entry["customType"]=="loop-tick")));
    controller.event(&ExtensionEvent::SessionShutdown(SessionShutdownEvent { reason:SessionReason::Quit,target_session_file:None,signal:None })).await.unwrap();
}
