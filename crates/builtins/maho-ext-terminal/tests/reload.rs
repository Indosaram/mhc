use maho_ext_api::*;
use maho_tools::definition::ToolCall;
use serde_json::json;
use std::{path::Path,sync::Arc};

struct Session;
impl ToolSessionManager for Session {
    fn session_id(&self)->&str {"terminal-reload-regression"}
    fn session_file(&self)->Option<&Path> {None}
}
impl SessionManager for Session {
    fn get_entries(&self)->Vec<SessionEntry> {vec![]}
    fn get_branch(&self)->Vec<SessionEntry> {vec![]}
    fn get_leaf_id(&self)->Option<String> {None}
    fn get_session_name(&self)->Option<String> {None}
}
struct Registry;
impl ModelRegistry for Registry {
    fn get_all(&self)->Vec<Model> {vec![]}
    fn get_available(&self)->Vec<Model> {vec![]}
    fn find(&self,_:&str,_:&str)->Option<Model> {None}
    fn has_configured_auth(&self,_:&Model)->bool {false}
    fn get_api_key_for_provider<'a>(&'a self,_:&'a str)->ExtensionFuture<'a,Option<String>> {Box::pin(async {Ok(None)})}
}
struct Ui;
impl ExtensionUi for Ui {
    fn select<'a>(&'a self,_:&'a str,_:&'a [String],_:ExtensionUiDialogOptions)->UiFuture<'a,Option<String>> {Box::pin(async {None})}
    fn confirm<'a>(&'a self,_:&'a str,_:&'a str,_:ExtensionUiDialogOptions)->UiFuture<'a,bool> {Box::pin(async {false})}
    fn input<'a>(&'a self,_:&'a str,_:Option<&'a str>,_:ExtensionUiDialogOptions)->UiFuture<'a,Option<String>> {Box::pin(async {None})}
    fn notify(&self,_:&str,_:NotificationType) {}
    fn set_status(&self,_:&str,_:Option<&str>) {}
    fn set_widget(&self,_:&str,_:Option<WidgetContent>,_:ExtensionWidgetOptions) {}
    fn set_header(&self,_:Option<ComponentFactory>) {}
    fn set_footer(&self,_:Option<ComponentFactory>) {}
    fn set_title(&self,_:&str) {}
    fn paste_to_editor(&self,_:&str) {}
    fn set_editor_text(&self,_:&str) {}
    fn get_editor_text(&self)->String {String::new()}
    fn custom(&self,_:ComponentFactory,_:CustomUiOptions)->ExtensionFuture<'_,JsonValue> {Box::pin(async {Err("unavailable".into())})}
    fn theme(&self)->Theme {Theme::default()}
}
fn context(dir:&Path)->ExtensionContext {
    ExtensionContext {ui:Arc::new(Ui),mode:ExtensionMode::Print,has_ui:false,cwd:dir.into(),agent_dir:dir.into(),session_manager:Arc::new(Session),model_registry:Arc::new(Registry),model:None,thinking_level:None,service_tier:None,effective_service_tier:None,scoped_models:vec![],goal_store_file:None,loaded_extension_paths:vec![],signal:None,steering_signal:None,is_idle_fn:Arc::new(||true),wait_for_idle_fn:Arc::new(||Box::pin(async {})),is_project_trusted_fn:Arc::new(||true),is_compacting_fn:Arc::new(||false),get_system_prompt_fn:Arc::new(String::new),get_system_prompt_options_fn:Arc::new(BuildSystemPromptOptions::default),registered_mcp_servers:vec![],update_tool_hook_status: None, idle_coordinator: None, logger: None, defer_macrotask: None,idle_coordinator:None,logger:None,defer_macrotask:None}
}
fn api(dir:&Path)->ExtensionApi {
    let mut api=ExtensionApi::new(LoadedExtension::new("terminal",dir.into(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),ExtensionRuntime::default());maho_ext_terminal::extension::TerminalExtension.register(&mut api);api
}
async fn shutdown(api:&ExtensionApi,ctx:&ExtensionContext,reason:SessionReason) {
    let mut event=ExtensionEvent::SessionShutdown(SessionShutdownEvent {reason,target_session_file:None,signal:None});
    for handler in &api.registered.handlers[&EventKind::SessionShutdown] {handler(&mut event,ctx).await.expect("shutdown handler");}
}
#[tokio::test]
async fn reload_preserves_live_pty_and_monitor_ids_until_new_owner_quits() {
    let dir=tempfile::tempdir().unwrap();let ctx=context(dir.path());let old=api(dir.path());
    let result=(old.registered.tools[5].definition.execute)(ToolCall {id:"watch",params:json!({"description":"live","command":"stty -echo; read value; printf '%s\\n' \"$value\"","persistent":true}),signal:Default::default(),on_update:None,context:None}).await.unwrap();let id=result.details.unwrap()["monitor_id"].as_str().unwrap().to_owned();
    let background=(old.registered.tools[0].definition.execute)(ToolCall {id:"background",params:json!({"command":"stty -echo; read value; printf 'background:%s\\n' \"$value\"","run_in_background":true}),signal:Default::default(),on_update:None,context:None}).await.unwrap();let background_id=background.details.unwrap()["bash_id"].as_str().unwrap().to_owned();
    shutdown(&old,&ctx,SessionReason::Reload).await;let new=api(dir.path());
    let mut event=ExtensionEvent::SessionStart(SessionStartEvent {reason:SessionReason::Reload,initial_model_provenance:None,previous_session_file:None});
    for handler in new.registered.handlers[&EventKind::SessionStart].iter().take(2) {handler(&mut event,&ctx).await.unwrap();}
    let (sender,mut states)=tokio::sync::mpsc::unbounded_channel();let _subscription=new.events.on("wake_source_state",Arc::new(move |state| {if state["source"]=="terminal-background-sessions" {sender.send(state.clone()).expect("background state");}}));
    new.registered.handlers[&EventKind::SessionStart].last().unwrap()(&mut event,&ctx).await.unwrap();assert_eq!(states.recv().await.unwrap()["activeCount"],1);
    (new.registered.tools[2].definition.execute)(ToolCall {id:"complete",params:json!({"bash_id":background_id,"input":"restored"}),signal:Default::default(),on_update:None,context:None}).await.unwrap();
    let completed=tokio::time::timeout(std::time::Duration::from_secs(5),states.recv()).await.unwrap().unwrap();assert_eq!(completed["activeCount"],0);
    let output=(new.registered.tools[1].definition.execute)(ToolCall {id:"peek",params:json!({"bash_id":id}),signal:Default::default(),on_update:None,context:None}).await.unwrap();assert!(output.content.iter().any(|part|matches!(part,maho_tools::definition::ToolContent::Text {text,..} if text.contains("status: running"))));
    let sent=(new.registered.tools[2].definition.execute)(ToolCall {id:"input",params:json!({"bash_id":id,"input":"restored"}),signal:Default::default(),on_update:None,context:None}).await.unwrap();assert!(!sent.content.is_empty());
    shutdown(&new,&ctx,SessionReason::Quit).await;
    assert!((new.registered.tools[1].definition.execute)(ToolCall {id:"after",params:json!({"bash_id":id}),signal:Default::default(),on_update:None,context:None}).await.is_err());
}
