use maho_ext_api::*;
use std::{sync::Arc,path::Path};
struct Session(Option<Arc<dyn ExtensionContextActions>>);
impl ToolSessionManager for Session { fn session_id(&self)->&str { "s" } fn session_file(&self)->Option<&Path> { None } }
impl SessionManager for Session {
    fn extension_context_actions(&self)->Option<&dyn ExtensionContextActions> { self.0.as_deref() }
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
#[derive(Default)]
pub struct Ui { pub statuses:std::sync::Mutex<Vec<(String,Option<String>)>>,pub selection:std::sync::Mutex<Option<String>>,pub choices:std::sync::Mutex<Vec<Vec<String>>> }
impl ExtensionUi for Ui {
    fn select<'a>(&'a self,_:&'a str,choices:&'a [String],_:ExtensionUiDialogOptions)->UiFuture<'a,Option<String>> { Box::pin(async move { self.choices.lock().unwrap().push(choices.to_vec()); self.selection.lock().unwrap().clone() }) }
    fn confirm<'a>(&'a self,_:&'a str,_:&'a str,_:ExtensionUiDialogOptions)->UiFuture<'a,bool> { Box::pin(async { false }) }
    fn input<'a>(&'a self,_:&'a str,_:Option<&'a str>,_:ExtensionUiDialogOptions)->UiFuture<'a,Option<String>> { Box::pin(async { None }) }
    fn notify(&self,_:&str,_:NotificationType) {}
    fn set_status(&self,key:&str,text:Option<&str>) { self.statuses.lock().unwrap().push((key.into(),text.map(str::to_owned))); }
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
pub fn context()->ExtensionContext {
    ExtensionContext { ui:Arc::new(Ui::default()),mode:ExtensionMode::Tui,has_ui:true,cwd:"/tmp".into(),agent_dir:"/tmp/agent".into(),session_manager:Arc::new(Session(None)),model_registry:Arc::new(Registry),model:None,thinking_level:None,service_tier:None,effective_service_tier:None,scoped_models:Vec::new(),goal_store_file:None,loaded_extension_paths:Vec::new(),signal:None,steering_signal:None,is_idle_fn:Arc::new(||true),wait_for_idle_fn:Arc::new(||Box::pin(async {})),is_project_trusted_fn:Arc::new(||true),is_compacting_fn:Arc::new(||false),get_system_prompt_fn:Arc::new(String::new),get_system_prompt_options_fn:Arc::new(BuildSystemPromptOptions::default),registered_mcp_servers:Vec::new(),update_tool_hook_status:None }
}
pub fn bind_session(context:&mut ExtensionContext)->maho_core::agent_session::AgentSession {
    use maho_core::{agent_session::*,settings_manager::*,session_manager::SessionManager,model_runtime::*};
    let stream:maho_agent::types::StreamFn=Arc::new(|_,_,_|maho_ai::types::AssistantMessageEventStream::assistant());
    let session=AgentSession::new(AgentSessionConfig { agent:maho_agent::Agent::new(maho_agent::AgentOptions { stream_fn:Some(stream),..Default::default() }),session_manager:SessionManager::in_memory("/tmp",None,None),settings_manager:SettingsManager::from_storage(Box::<InMemorySettingsStorage>::default(),false),cwd:"/tmp".into(),agent_dir:Some("/tmp/maho-agent".into()),fallback_now:None,retry_random:None,scoped_models:Vec::new(),favorite_models:Vec::new(),flag_values:Default::default(),custom_tools:Vec::new(),model_runtime:Some(ModelRuntime::create_sync(CreateModelRuntimeOptions { providers:Some(Vec::new()),..Default::default() })),model_registry:None,uses_default_stream_function:Some(false),initial_active_tool_names:None,default_tool_names:None,eval_only_tool_names:None,allowed_tool_names:None,excluded_tool_names:None,base_tools_override:None,session_start_event:None,auto_title_sessions:Some(false) }).unwrap();
    context.session_manager=Arc::new(Session(Some(session.extension_context_actions()))); session
}
