use std::sync::Arc;
use maho_ext_api::*;
use maho_codemode::tool::image_resize::*;

struct Images;

struct CompletionDrop(Option<tokio::sync::oneshot::Sender<()>>);
impl Drop for CompletionDrop {
    fn drop(&mut self) { if let Some(sender)=self.0.take() {let _=sender.send(());} }
}

#[tokio::test]
async fn completion_ignoring_abort_is_dropped_before_session_shutdown_returns() {
    let root=tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join(".maho")).unwrap();
    std::fs::write(root.path().join(".maho/codemode.json"),r#"{"languages":{"py":true,"js":false,"rb":false,"jl":false}}"#).unwrap();
    let host=Arc::new(Host::default());
    let runtime=ExtensionRuntime::default();runtime.bind(host.clone());runtime.bind_session_actions(host.clone());
    let mut api=ExtensionApi::new(LoadedExtension::new("codemode",root.path().into(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),runtime);
    let (started,mut started_rx)=tokio::sync::mpsc::unbounded_channel();
    let (dropped,dropped_rx)=tokio::sync::oneshot::channel();let dropped=Arc::new(Mutex::new(Some(dropped)));
    maho_codemode::register(&mut api,maho_codemode::CodemodeExtensionOptions {image_sdk:Arc::new(Images),complete:Arc::new(move |_,ctx| {let guard=CompletionDrop(dropped.lock().unwrap().take());let started=started.clone();Box::pin(async move {let _guard=guard;started.send(ctx.signal.unwrap()).unwrap();std::future::pending().await})}),home_dir:root.path().into(),environment:Default::default(),js_runtime:maho_codemode::tool::types::EvalRuntimeInfo {name:"bun".into(),version:"1.4.0".into(),path:None}}).unwrap();
    let ctx=context(root.path());
    let mut start=ExtensionEvent::SessionStart(SessionStartEvent {reason:SessionReason::Startup,initial_model_provenance:None,previous_session_file:None});
    (api.registered.handlers[&EventKind::SessionStart][0])(&mut start,&ctx).await.unwrap();
    let execute=host.tools.lock().unwrap()[0].definition.execute.clone();
    let signal=maho_tools::definition::AbortSignal::default();let caller_signal=signal.clone();
    let invocation_context=ctx.clone();
    let mut run=tokio::spawn(async move {execute(maho_tools::definition::ToolCall {id:"ignored-completion",params:serde_json::json!({"language":"py","code":"completion('park')","summary":"completion cancellation","on_timeout":"error"}),signal:caller_signal,on_update:None,context:Some(&invocation_context)}).await});
    let started=tokio::time::timeout(std::time::Duration::from_secs(10),started_rx.recv()).await;
    signal.abort();
    let cancellation=tokio::time::timeout(std::time::Duration::from_secs(3),&mut run).await;
    if cancellation.is_err() {run.abort();let _=run.await;}
    let mut shutdown=ExtensionEvent::SessionShutdown(SessionShutdownEvent {reason:SessionReason::Quit,target_session_file:None,signal:None});
    let shutdown=tokio::time::timeout(std::time::Duration::from_secs(3),(api.registered.handlers[&EventKind::SessionShutdown][0])(&mut shutdown,&ctx)).await;
    let dropped=tokio::time::timeout(std::time::Duration::from_secs(3),dropped_rx).await;
    assert!(started.unwrap().unwrap().is_aborted());
    assert!(cancellation.is_ok(),"caller did not settle after abort");
    assert!(shutdown.is_ok(),"session shutdown blocked behind completion ignoring abort");
    shutdown.unwrap().unwrap();
    dropped.expect("completion future must be dropped").unwrap();
}
impl EvalImageSdk for Images {
    fn resize_image<'a>(&'a self, _:Vec<u8>, _:&'a str, _:Option<usize>) -> ImageFuture<'a,Option<ResizedImage>> { Box::pin(async {panic!("factory setup must not resize images")}) }
    fn convert_to_png<'a>(&'a self, _:&'a str, _:&'a str) -> ImageFuture<'a,Option<EvalImageContent>> { Box::pin(async {panic!("factory setup must not convert images")}) }
}

#[tokio::test]
async fn native_factory_registers_baseline_eval_before_runtime_actions_are_bound() {
    let mut api=ExtensionApi::new(LoadedExtension::new("codemode","/fixture".into(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),ExtensionRuntime::default());
    maho_codemode::register(&mut api, maho_codemode::CodemodeExtensionOptions {
        image_sdk:Arc::new(Images),
        complete:Arc::new(|_,_|Box::pin(async {panic!("factory setup must not request completion")})),
        home_dir:"/fixture/home".into(),
        environment:Default::default(),
        js_runtime:maho_codemode::tool::types::EvalRuntimeInfo {name:"bun".into(),version:"1.4.0".into(),path:None},
    }).unwrap();
    assert_eq!(api.registered.tools.len(),1);
    assert_eq!(api.registered.tools[0].definition.name,"eval");
    for event in [EventKind::ResourcesDiscover,EventKind::SessionStart,EventKind::SessionShutdown,EventKind::SessionBeforeSwitch,EventKind::SessionBeforeFork,EventKind::ModelSelect] {
        assert_eq!(api.registered.handlers[&event].len(),1);
    }
    assert!(api.registered.removed_tool_hints.contains_key("exec"));
    assert!(api.registered.removed_tool_hints.contains_key("wait"));
    let result=(api.registered.tools[0].definition.execute)(maho_tools::definition::ToolCall {id:"before-start",params:serde_json::json!({"language":"js","code":"42","summary":"admission proof"}),signal:Default::default(),on_update:None,context:None}).await;
    assert!(result.unwrap_err().to_string().contains("has not started"));
}

use std::{path::Path, sync::Mutex};
struct Session(std::path::PathBuf);
impl ToolSessionManager for Session {fn session_id(&self)->&str {"registered-session"} fn session_file(&self)->Option<&Path> {Some(&self.0)}}
impl SessionManager for Session {fn get_entries(&self)->Vec<SessionEntry> {vec![]} fn get_branch(&self)->Vec<SessionEntry> {vec![]} fn get_leaf_id(&self)->Option<String> {None} fn get_session_name(&self)->Option<String> {None}}
struct Registry;

#[tokio::test]
async fn registered_callable_enforces_configured_detached_capacity() {
    let root=tempfile::tempdir().unwrap();std::fs::create_dir(root.path().join(".maho")).unwrap();
    std::fs::write(root.path().join(".maho/codemode.json"),r#"{"languages":{"py":false,"js":true,"rb":false,"jl":false},"maxDetachedCells":1,"cellTimeoutSeconds":0.01,"foregroundWindowSeconds":0.1}"#).unwrap();
    let host=Arc::new(Host::default());let runtime=ExtensionRuntime::default();runtime.bind(host.clone());runtime.bind_session_actions(host.clone());
    let mut api=ExtensionApi::new(LoadedExtension::new("codemode",root.path().into(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),runtime);
    maho_codemode::register(&mut api,maho_codemode::CodemodeExtensionOptions {image_sdk:Arc::new(Images),complete:Arc::new(|_,_|Box::pin(async {panic!("capacity does not use completion")})),home_dir:root.path().into(),environment:Default::default(),js_runtime:maho_codemode::tool::types::EvalRuntimeInfo {name:"bun".into(),version:"1.4.0".into(),path:None}}).unwrap();
    let ctx=context(root.path());let mut start=ExtensionEvent::SessionStart(SessionStartEvent {reason:SessionReason::Startup,initial_model_provenance:None,previous_session_file:None});
    (api.registered.handlers[&EventKind::SessionStart][0])(&mut start,&ctx).await.unwrap();
    let execute=host.tools.lock().unwrap().last().unwrap().definition.execute.clone();
    let run=|id|execute(maho_tools::definition::ToolCall {id,params:serde_json::json!({"language":"js","code":"await new Promise(() => {})","summary":"capacity","on_timeout":"detach"}),signal:Default::default(),on_update:None,context:Some(&ctx)});
    let first=tokio::time::timeout(std::time::Duration::from_secs(5),run("capacity-first")).await;
    let second=tokio::time::timeout(std::time::Duration::from_secs(5),run("capacity-second")).await;
    let mut shutdown=ExtensionEvent::SessionShutdown(SessionShutdownEvent {reason:SessionReason::Quit,target_session_file:None,signal:None});
    (api.registered.handlers[&EventKind::SessionShutdown][0])(&mut shutdown,&ctx).await.unwrap();
    assert_eq!(first.unwrap().unwrap().details.unwrap()["cells"][0]["status"],"detached");
    let second=second.unwrap().unwrap().details.unwrap();
    assert_eq!(second["isError"],true);
    assert_eq!(second["cells"][0]["status"],"error");
}

#[tokio::test]
async fn registered_callable_schema_uses_resolved_language_settings() {
    let root=tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join(".maho")).unwrap();
    std::fs::write(root.path().join(".maho/codemode.json"),r#"{"languages":{"py":true,"js":false,"rb":false,"jl":false},"maxDetachedCells":3}"#).unwrap();
    let host=Arc::new(Host::default());let runtime=ExtensionRuntime::default();runtime.bind(host.clone());runtime.bind_session_actions(host.clone());
    let mut api=ExtensionApi::new(LoadedExtension::new("codemode",root.path().into(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),runtime);
    maho_codemode::register(&mut api,maho_codemode::CodemodeExtensionOptions {image_sdk:Arc::new(Images),complete:Arc::new(|_,_|Box::pin(async {panic!("capacity does not use completion")})),home_dir:root.path().into(),environment:Default::default(),js_runtime:maho_codemode::tool::types::EvalRuntimeInfo {name:"bun".into(),version:"1.4.0".into(),path:None}}).unwrap();
    let ctx=context(root.path());let mut start=ExtensionEvent::SessionStart(SessionStartEvent {reason:SessionReason::Startup,initial_model_provenance:None,previous_session_file:None});
    (api.registered.handlers[&EventKind::SessionStart][0])(&mut start,&ctx).await.unwrap();
    let schema=host.tools.lock().unwrap().last().unwrap().definition.parameters.clone();
    let mut shutdown=ExtensionEvent::SessionShutdown(SessionShutdownEvent {reason:SessionReason::Quit,target_session_file:None,signal:None});
    (api.registered.handlers[&EventKind::SessionShutdown][0])(&mut shutdown,&ctx).await.unwrap();
    assert_eq!(schema["properties"]["language"]["anyOf"],serde_json::json!([{"const":"py","type":"string"}]));
}

#[tokio::test]
async fn concurrent_completion_keeps_each_invocations_context() {
    let root=tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join(".maho")).unwrap();
    std::fs::write(root.path().join(".maho/codemode.json"),r#"{"languages":{"py":false,"js":true,"rb":false,"jl":false}}"#).unwrap();
    let host=Arc::new(Host::default());let runtime=ExtensionRuntime::default();runtime.bind(host.clone());runtime.bind_session_actions(host.clone());
    let mut api=ExtensionApi::new(LoadedExtension::new("codemode",root.path().into(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),runtime);
    let (entered,mut entries)=tokio::sync::mpsc::unbounded_channel();
    let (release,released)=tokio::sync::oneshot::channel();let released=Arc::new(Mutex::new(Some(released)));
    let seen=Arc::new(Mutex::new(Vec::new()));let captured=seen.clone();
    maho_codemode::register(&mut api,maho_codemode::CodemodeExtensionOptions {image_sdk:Arc::new(Images),complete:Arc::new(move |request,ctx| {
        let wait=if request.prompt=="A" {released.lock().unwrap().take()} else {None};let entered=entered.clone();let captured=captured.clone();
        Box::pin(async move {entered.send(request.prompt.clone()).unwrap();if let Some(wait)=wait {wait.await.unwrap();}captured.lock().unwrap().push((request.prompt,ctx.cwd,ctx.thinking_level,ctx.goal_store_file,ctx.model.map(|model|model.name),ctx.service_tier));Ok(serde_json::json!({"text":"ok"}))})
    }),home_dir:root.path().into(),environment:Default::default(),js_runtime:maho_codemode::tool::types::EvalRuntimeInfo {name:"bun".into(),version:"1.4.0".into(),path:None}}).unwrap();
    let ctx=context(root.path());let mut start=ExtensionEvent::SessionStart(SessionStartEvent {reason:SessionReason::Startup,initial_model_provenance:None,previous_session_file:None});
    (api.registered.handlers[&EventKind::SessionStart][0])(&mut start,&ctx).await.unwrap();
    let execute=host.tools.lock().unwrap()[0].definition.execute.clone();
    let mut a=ctx.clone();a.cwd=root.path().join("A");a.thinking_level=Some(ThinkingLevel::Low);a.goal_store_file=Some(root.path().join("A.goal"));
    let mut b=ctx.clone();b.cwd=root.path().join("B");b.thinking_level=Some(ThinkingLevel::High);b.goal_store_file=Some(root.path().join("B.goal"));
    let first=execute(maho_tools::definition::ToolCall {id:"context-A",params:serde_json::json!({"language":"js","code":"await completion('A')","summary":"A","on_timeout":"error"}),signal:Default::default(),on_update:None,context:Some(&a)});
    let second=execute(maho_tools::definition::ToolCall {id:"context-B",params:serde_json::json!({"language":"js","code":"await completion('B')","summary":"B","on_timeout":"error"}),signal:Default::default(),on_update:None,context:Some(&b)});
    tokio::pin!(first);tokio::pin!(second);
    tokio::time::timeout(std::time::Duration::from_secs(10),async {tokio::select! {entry=entries.recv()=>assert_eq!(entry.as_deref(),Some("A")),result=&mut first=>panic!("A settled before barrier: {result:?}")}}).await.unwrap();
    assert!(std::future::poll_fn(|cx|std::task::Poll::Ready(second.as_mut().poll(cx))).await.is_pending());
    release.send(()).unwrap();
    let results=tokio::time::timeout(std::time::Duration::from_secs(10),async {tokio::join!(&mut first,&mut second)}).await;
    let mut shutdown=ExtensionEvent::SessionShutdown(SessionShutdownEvent {reason:SessionReason::Quit,target_session_file:None,signal:None});
    (api.registered.handlers[&EventKind::SessionShutdown][0])(&mut shutdown,&ctx).await.unwrap();
    let (first,second)=results.unwrap();first.unwrap();second.unwrap();
    assert_eq!(*seen.lock().unwrap(),vec![("A".into(),a.cwd.clone(),a.thinking_level,a.goal_store_file.clone(),None,None),("B".into(),b.cwd.clone(),b.thinking_level,b.goal_store_file.clone(),None,None)]);
}
impl ModelRegistry for Registry {
    fn get_all(&self)->Vec<Model> {vec![]} fn get_available(&self)->Vec<Model> {vec![]} fn find(&self,_:&str,_:&str)->Option<Model> {None} fn has_configured_auth(&self,_:&Model)->bool {false}
    fn get_api_key_for_provider<'a>(&'a self,_:&'a str)->ExtensionFuture<'a,Option<String>> {Box::pin(async {Ok(None)})}
}
struct Ui;
impl ExtensionUi for Ui {
    fn select<'a>(&'a self,_:&'a str,_:&'a [String],_:ExtensionUiDialogOptions)->UiFuture<'a,Option<String>> {Box::pin(async {None})}
    fn confirm<'a>(&'a self,_:&'a str,_:&'a str,_:ExtensionUiDialogOptions)->UiFuture<'a,bool> {Box::pin(async {false})}
    fn input<'a>(&'a self,_:&'a str,_:Option<&'a str>,_:ExtensionUiDialogOptions)->UiFuture<'a,Option<String>> {Box::pin(async {None})}
    fn notify(&self,_:&str,_:NotificationType) {} fn set_status(&self,_:&str,_:Option<&str>) {} fn set_widget(&self,_:&str,_:Option<WidgetContent>,_:ExtensionWidgetOptions) {} fn set_header(&self,_:Option<ComponentFactory>) {} fn set_footer(&self,_:Option<ComponentFactory>) {} fn set_title(&self,_:&str) {} fn paste_to_editor(&self,_:&str) {} fn set_editor_text(&self,_:&str) {} fn get_editor_text(&self)->String {String::new()}
    fn custom(&self,_:ComponentFactory,_:CustomUiOptions)->ExtensionFuture<'_,JsonValue> {Box::pin(async {Err("headless".into())})} fn theme(&self)->Theme {Theme::default()}
}
fn context(root:&Path)->ExtensionContext {
    ExtensionContext {ui:Arc::new(Ui),mode:ExtensionMode::Print,has_ui:false,cwd:root.into(),agent_dir:root.join("agent"),session_manager:Arc::new(Session(root.join("session.jsonl"))),model_registry:Arc::new(Registry),model:None,thinking_level:None,service_tier:None,effective_service_tier:None,scoped_models:vec![],goal_store_file:None,loaded_extension_paths:vec![],signal:None,steering_signal:None,is_idle_fn:Arc::new(||true),wait_for_idle_fn:Arc::new(||Box::pin(async {})),is_project_trusted_fn:Arc::new(||true),is_compacting_fn:Arc::new(||false),get_system_prompt_fn:Arc::new(String::new),get_system_prompt_options_fn:Arc::new(BuildSystemPromptOptions::default),registered_mcp_servers:vec![],update_tool_hook_status: None, idle_coordinator: None, logger: None, defer_macrotask: None}
}
#[derive(Default)]
struct Host {tools:Mutex<Vec<RegisteredTool>>}
impl ExtensionActions for Host {
    fn send_message(&self,_:CustomMessage,_:SendMessageOptions)->Result<(),ExtensionFailure> {Ok(())}
    fn send_user_message(&self,_:UserMessageContent,_:SendUserMessageOptions)->Result<(),ExtensionFailure> {Ok(())}
    fn append_entry(&self,_:&str,_:Option<JsonValue>)->Result<(),ExtensionFailure> {Ok(())}
    fn get_all_tools(&self)->Result<Vec<ToolInfo>,ExtensionFailure> {Ok(self.tools.lock().map_err(|error|ExtensionFailure::new(error.to_string()))?.iter().map(|tool|ToolInfo {name:tool.definition.name.clone(),label:tool.definition.label.clone(),description:tool.definition.description.clone(),parameters:tool.definition.parameters.clone(),prompt_guidelines:tool.definition.prompt_guidelines.clone(),source_info:tool.source_info.clone(),exposure:ToolExposure::Direct,search_text:None,search_keywords:vec![],search_group:None,allow_lazy_activation:false}).collect())}
}

#[tokio::test]
async fn completion_context_refreshes_for_same_model_id_without_reinstalling_eval() {
    let root=tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join(".maho")).unwrap();
    std::fs::write(root.path().join(".maho/codemode.json"),r#"{"languages":{"py":true,"js":true,"rb":true,"jl":true}}"#).unwrap();
    let host=Arc::new(Host::default());
    let runtime=ExtensionRuntime::default();runtime.bind(host.clone());runtime.bind_session_actions(host.clone());
    let mut api=ExtensionApi::new(LoadedExtension::new("codemode",root.path().into(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),runtime);
    let seen=Arc::new(Mutex::new(Vec::new()));let captured=seen.clone();
    maho_codemode::register(&mut api,maho_codemode::CodemodeExtensionOptions {image_sdk:Arc::new(Images),complete:Arc::new(move |_,ctx|{let captured=captured.clone();Box::pin(async move {captured.lock().unwrap().push((ctx.service_tier,ctx.model.as_ref().map(|model|model.name.clone())));Ok(serde_json::json!({"text":"fresh"}))})}),home_dir:root.path().into(),environment:Default::default(),js_runtime:maho_codemode::tool::types::EvalRuntimeInfo {name:"bun".into(),version:"1.4.0".into(),path:None}}).unwrap();
    let mut ctx=context(root.path());
    let model:Model=serde_json::from_value(serde_json::json!({"id":"same","name":"same","provider":"fixture","api":"openai-responses","baseUrl":"http://localhost","reasoning":false,"input":["text"],"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":4096,"maxTokens":100})).unwrap();
    ctx.model=Some(model.clone());
    let mut start=ExtensionEvent::SessionStart(SessionStartEvent {reason:SessionReason::Startup,initial_model_provenance:None,previous_session_file:None});
    (api.registered.handlers[&EventKind::SessionStart][0])(&mut start,&ctx).await.unwrap();
    let execute=host.tools.lock().unwrap()[0].definition.execute.clone();
    ctx.service_tier=Some(ServiceTier::Priority);
    let mut selected=ExtensionEvent::ModelSelect(ModelSelectEvent {model,previous_model:ctx.model.clone(),source:ModelSelectSource::Set,system_prompt:String::new(),system_prompt_options:Default::default()});
    (api.registered.handlers[&EventKind::ModelSelect][0])(&mut selected,&ctx).await.unwrap();
    assert!(Arc::ptr_eq(&execute,&host.tools.lock().unwrap()[0].definition.execute));
    ctx.model.as_mut().unwrap().name="invocation-local".into();
    let mut results=Vec::new();
    for (id,language,code) in [("fresh-js","js","await completion('context')"),("fresh-py","py","completion('context')"),("fresh-rb","rb","completion('context')"),("fresh-jl","jl","completion(\"context\")")] {
        results.push(tokio::time::timeout(std::time::Duration::from_secs(10),execute(maho_tools::definition::ToolCall {id,params:serde_json::json!({"language":language,"code":code,"summary":"context freshness","on_timeout":"error"}),signal:Default::default(),on_update:None,context:Some(&ctx)})).await);
    }
    let mut shutdown=ExtensionEvent::SessionShutdown(SessionShutdownEvent {reason:SessionReason::Quit,target_session_file:None,signal:None});
    tokio::time::timeout(std::time::Duration::from_secs(10),(api.registered.handlers[&EventKind::SessionShutdown][0])(&mut shutdown,&ctx)).await.unwrap().unwrap();
    for result in results {let result=result.unwrap().unwrap();assert_ne!(result.details.as_ref().unwrap()["isError"],true,"{result:?}");}
    assert_eq!(*seen.lock().unwrap(),vec![(Some(ServiceTier::Priority),Some("invocation-local".into())),(Some(ServiceTier::Priority),Some("invocation-local".into())),(Some(ServiceTier::Priority),Some("invocation-local".into())),(Some(ServiceTier::Priority),Some("invocation-local".into()))]);
    eprintln!("cleanup: completion freshness JS, Python, Ruby and Julia managers disposed");
}
impl ExtensionSessionActions for Host {
    fn set_session_name(&self,_:&str)->Result<(),ExtensionFailure> {Ok(())} fn get_session_name(&self)->Result<Option<String>,ExtensionFailure> {Ok(None)} fn set_label(&self,_:&str,_:Option<&str>)->Result<(),ExtensionFailure> {Ok(())}
    fn execute_tool<'a>(&'a self,name:&'a str,_:JsonValue,_:ExecuteToolOptions)->ExecuteToolFuture<'a> {Box::pin(async move {Err(ExecuteToolError {code:ExecuteToolErrorCode::UnknownTool,tool_name:name.into(),message:"unknown fixture tool".into(),active_tools:vec![]})})}
    fn get_active_tools(&self)->Result<Vec<String>,ExtensionFailure> {Ok(vec!["eval".into()])} fn set_active_tools(&self,_:Vec<String>)->Result<(),ExtensionFailure> {Ok(())} fn refresh_tools(&self)->Result<(),ExtensionFailure> {Ok(())}
    fn install_registered_tool(&self,tool:RegisteredTool)->Result<(),ExtensionFailure> {let mut tools=self.tools.lock().map_err(|error|ExtensionFailure::new(error.to_string()))?;if let Some(old)=tools.iter_mut().find(|old|old.definition.name==tool.definition.name) {*old=tool;} else {tools.push(tool);} Ok(())}
    fn register_removed_tool_hint(&self,_:&str,_:&str)->Result<(),ExtensionFailure> {Ok(())} fn register_lazy_tool_activator(&self,_:LazyToolActivator)->Result<(),ExtensionFailure> {Ok(())} fn get_commands(&self)->Result<Vec<SlashCommandInfo>,ExtensionFailure> {Ok(vec![])}
    fn set_model(&self,_:Model)->ExtensionFuture<'_,bool> {Box::pin(async {Ok(true)})} fn get_thinking_level(&self)->Result<ThinkingLevel,ExtensionFailure> {Ok(ThinkingLevel::Minimal)} fn set_thinking_level(&self,_:ThinkingLevel)->Result<(),ExtensionFailure> {Ok(())}
    fn set_session_model(&self,_:Model)->ExtensionFuture<'_,bool> {Box::pin(async {Ok(true)})} fn set_session_thinking_level(&self,_:ThinkingLevel)->Result<(),ExtensionFailure> {Ok(())} fn set_session_fast_mode(&self,_:bool)->Result<(),ExtensionFailure> {Ok(())}
    fn exec<'a>(&'a self,_:&'a str,_:&'a [String],_:&'a Path,_:ExecOptions)->ExtensionFuture<'a,ExecResult> {Box::pin(async {panic!("unexpected host exec")})}
}

#[tokio::test]
async fn native_session_registration_runs_real_worker_and_shutdown_rejects_stale_runs() {
    let root=tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join(".maho")).unwrap();
    std::fs::write(root.path().join(".maho/codemode.json"),r#"{"languages":{"py":false,"js":true,"rb":false,"jl":false}}"#).unwrap();
    let host=Arc::new(Host::default());
    let runtime=ExtensionRuntime::default();runtime.bind(host.clone());runtime.bind_session_actions(host.clone());
    let mut api=ExtensionApi::new(LoadedExtension::new("codemode",root.path().into(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),runtime);
    maho_codemode::register(&mut api,maho_codemode::CodemodeExtensionOptions {image_sdk:Arc::new(Images),complete:Arc::new(|_,_|Box::pin(async {panic!("unexpected completion")})),home_dir:root.path().into(),environment:Default::default(),js_runtime:maho_codemode::tool::types::EvalRuntimeInfo {name:"bun".into(),version:"1.4.0".into(),path:None}}).unwrap();
    let ctx=context(root.path());
    let emissions=Arc::new(Mutex::new(Vec::new()));
    let captured=emissions.clone();
    let _settled=api.events.on(maho_codemode::tool::eval_execution_event::EVAL_EXECUTION_EVENT,Arc::new(move |value|captured.lock().unwrap().push(value.clone())));
    let rpc=Arc::new(Mutex::new(Vec::new()));let captured=rpc.clone();
    let _rpc=api.events.on("senpi:extension-rpc-event",Arc::new(move |value|captured.lock().unwrap().push(value.clone())));
    let mut start=ExtensionEvent::SessionStart(SessionStartEvent {reason:SessionReason::Startup,initial_model_provenance:None,previous_session_file:None});
    tokio::time::timeout(std::time::Duration::from_secs(10),(api.registered.handlers[&EventKind::SessionStart][0])(&mut start,&ctx)).await.unwrap().unwrap();
    let execute=host.tools.lock().unwrap()[0].definition.execute.clone();
    let run=tokio::time::timeout(std::time::Duration::from_secs(10),execute(maho_tools::definition::ToolCall {id:"registered-real",params:serde_json::json!({"language":"js","code":"JSON.stringify({value:process.env.PI_SESSION_ID + ':' + (6*7),pid:process.pid})","summary":"registered worker proof","on_timeout":"error"}),signal:Default::default(),on_update:None,context:Some(&ctx)})).await;
    let mut shutdown=ExtensionEvent::SessionShutdown(SessionShutdownEvent {reason:SessionReason::Quit,target_session_file:None,signal:None});
    tokio::time::timeout(std::time::Duration::from_secs(10),(api.registered.handlers[&EventKind::SessionShutdown][0])(&mut shutdown,&ctx)).await.unwrap().unwrap();
    let result=run.unwrap().unwrap();
    let value: String=serde_json::from_str(result.details.as_ref().unwrap()["cells"][0]["output"].as_str().unwrap()).unwrap();
    let value:JsonValue=serde_json::from_str(&value).unwrap();
    assert_eq!(value["value"],"registered-session:42");
    assert_eq!(emissions.lock().unwrap().len(),1);
    assert_eq!(emissions.lock().unwrap()[0]["cellId"],"registered-real");
    assert_eq!(rpc.lock().unwrap().iter().filter(|event|event["name"]==maho_codemode::tool::eval_execution_event::EVAL_EXECUTION_EVENT).count(),1);
    let pid=value["pid"].as_u64().unwrap();
    assert!(!Path::new(&format!("/proc/{pid}")).exists(),"registered worker still alive after shutdown");
    let result=execute(maho_tools::definition::ToolCall {id:"stale",params:serde_json::json!({"language":"js","code":"42","summary":"stale"}),signal:Default::default(),on_update:None,context:Some(&ctx)}).await;
    assert!(result.unwrap_err().to_string().contains("disposed"));
    eprintln!("cleanup: registered session shutdown awaited manager; worker pid {pid} absent");
}
