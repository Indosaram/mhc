use maho_ext_api::*;
use std::{path::Path,sync::{Arc,Mutex}};
struct Session;
impl ToolSessionManager for Session { fn session_id(&self)->&str { "session" } fn session_file(&self)->Option<&Path> { None } }
impl SessionManager for Session { fn get_entries(&self)->Vec<SessionEntry> { vec![] } fn get_branch(&self)->Vec<SessionEntry> { vec![] } fn get_leaf_id(&self)->Option<String> { None } fn get_session_name(&self)->Option<String> { None } }
struct Registry;
impl ModelRegistry for Registry {
    fn get_all(&self)->Vec<Model> { vec![] } fn get_available(&self)->Vec<Model> { vec![] } fn find(&self,_:&str,_:&str)->Option<Model> { None } fn has_configured_auth(&self,_:&Model)->bool { false }
    fn get_api_key_for_provider<'a>(&'a self,_:&'a str)->ExtensionFuture<'a,Option<String>> { Box::pin(async { Ok(None) }) }
}
#[derive(Default)]
pub struct Ui {
    pub select_first:bool,
    pub confirmed:bool,
    pub selections:Mutex<Vec<Vec<String>>>,
    pub notifications:Mutex<Vec<String>>,
    pub widgets:Mutex<Vec<(Option<WidgetContent>,WidgetPlacement)>>,
}
impl ExtensionUi for Ui {
    fn select<'a>(&'a self,_:&'a str,options:&'a [String],_:ExtensionUiDialogOptions)->UiFuture<'a,Option<String>> { self.selections.lock().expect("selections").push(options.to_vec()); Box::pin(async move { self.select_first.then(|| options.first().cloned()).flatten() }) }
    fn confirm<'a>(&'a self,_:&'a str,_:&'a str,_:ExtensionUiDialogOptions)->UiFuture<'a,bool> { Box::pin(async move { self.confirmed }) }
    fn input<'a>(&'a self,_:&'a str,_:Option<&'a str>,_:ExtensionUiDialogOptions)->UiFuture<'a,Option<String>> { Box::pin(async { None }) }
    fn notify(&self,message:&str,_:NotificationType) { self.notifications.lock().expect("notifications").push(message.to_owned()); } fn set_status(&self,_:&str,_:Option<&str>) {} fn set_widget(&self,_:&str,content:Option<WidgetContent>,options:ExtensionWidgetOptions) { self.widgets.lock().expect("widgets").push((content,options.placement)); }
    fn set_header(&self,_:Option<ComponentFactory>) {} fn set_footer(&self,_:Option<ComponentFactory>) {} fn set_title(&self,_:&str) {} fn paste_to_editor(&self,_:&str) {} fn set_editor_text(&self,_:&str) {} fn get_editor_text(&self)->String { String::new() }
    fn custom(&self,_:ComponentFactory,_:CustomUiOptions)->ExtensionFuture<'_,JsonValue> { Box::pin(async { Err(ExtensionFailure::new("no UI")) }) }
    fn theme(&self)->Theme { Theme::default() }
}
pub fn context()->ExtensionContext {
    ExtensionContext { ui:Arc::new(Ui::default()),mode:ExtensionMode::Print,has_ui:false,cwd:"/tmp".into(),agent_dir:"/tmp/agent".into(),session_manager:Arc::new(Session),model_registry:Arc::new(Registry),model:None,thinking_level:None,service_tier:None,effective_service_tier:None,scoped_models:vec![],goal_store_file:None,loaded_extension_paths:vec![],signal:None,steering_signal:None,is_idle_fn:Arc::new(|| true),wait_for_idle_fn:Arc::new(|| Box::pin(async {})),is_project_trusted_fn:Arc::new(|| true),is_compacting_fn:Arc::new(|| false),get_system_prompt_fn:Arc::new(String::new),get_system_prompt_options_fn:Arc::new(BuildSystemPromptOptions::default),registered_mcp_servers:vec![],update_tool_hook_status: None, idle_coordinator: None, logger: None, defer_macrotask: None }
}
pub fn api()->ExtensionApi { ExtensionApi::new(LoadedExtension::new("task","/tmp".into(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),ExtensionRuntime::default()) }
