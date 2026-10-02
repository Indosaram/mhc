use std::{collections::BTreeMap,path::{Path,PathBuf},sync::Arc};
use maho_ext_api::*;
struct Session(String);
impl ToolSessionManager for Session {fn session_id(&self)->&str {&self.0} fn session_file(&self)->Option<&Path> {None}}
impl SessionManager for Session {fn get_entries(&self)->Vec<SessionEntry> {Vec::new()} fn get_branch(&self)->Vec<SessionEntry> {Vec::new()} fn get_leaf_id(&self)->Option<String> {None} fn get_session_name(&self)->Option<String> {None}}
struct Models;
impl ModelRegistry for Models {fn get_all(&self)->Vec<Model> {Vec::new()} fn get_available(&self)->Vec<Model> {Vec::new()} fn find(&self,_:&str,_:&str)->Option<Model> {None} fn has_configured_auth(&self,_:&Model)->bool {false} fn get_api_key_for_provider<'a>(&'a self,_:&'a str)->ExtensionFuture<'a,Option<String>> {Box::pin(async {Ok(None)})}}
#[derive(Default)]
struct Ui {notifications:Arc<std::sync::Mutex<Vec<String>>>}
impl ExtensionUi for Ui {
    fn select<'a>(&'a self,_:&'a str,_:&'a [String],_:ExtensionUiDialogOptions)->UiFuture<'a,Option<String>> {Box::pin(async {None})}
    fn confirm<'a>(&'a self,_:&'a str,_:&'a str,_:ExtensionUiDialogOptions)->UiFuture<'a,bool> {Box::pin(async {false})}
    fn input<'a>(&'a self,_:&'a str,_:Option<&'a str>,_:ExtensionUiDialogOptions)->UiFuture<'a,Option<String>> {Box::pin(async {None})}
    fn notify(&self,message:&str,_:NotificationType) {self.notifications.lock().unwrap().push(message.into());}
    fn set_status(&self,_:&str,_:Option<&str>) {}
    fn set_widget(&self,_:&str,_:Option<WidgetContent>,_:ExtensionWidgetOptions) {}
    fn set_header(&self,_:Option<ComponentFactory>) {}
    fn set_footer(&self,_:Option<ComponentFactory>) {}
    fn set_title(&self,_:&str) {}
    fn paste_to_editor(&self,_:&str) {}
    fn set_editor_text(&self,_:&str) {}
    fn get_editor_text(&self)->String {String::new()}
    fn custom(&self,_:ComponentFactory,_:CustomUiOptions)->ExtensionFuture<'_,serde_json::Value> {Box::pin(async {Ok(serde_json::Value::Null)})}
    fn theme(&self)->Theme {Theme::default()}
}
pub fn context(home:&Path,id:&str)->ExtensionContext {ExtensionContext {ui:Arc::new(Ui::default()),mode:ExtensionMode::Tui,has_ui:true,cwd:home.into(),agent_dir:home.into(),session_manager:Arc::new(Session(id.into())),model_registry:Arc::new(Models),model:None,thinking_level:None,service_tier:None,effective_service_tier:None,scoped_models:Vec::new(),goal_store_file:None,loaded_extension_paths:Vec::new(),signal:None,steering_signal:None,is_idle_fn:Arc::new(||true),wait_for_idle_fn:Arc::new(||Box::pin(async {})),is_project_trusted_fn:Arc::new(||true),is_compacting_fn:Arc::new(||false),get_system_prompt_fn:Arc::new(String::new),get_system_prompt_options_fn:Arc::new(BuildSystemPromptOptions::default),registered_mcp_servers:Vec::new(),update_tool_hook_status:None}}
pub fn context_with_notifications(home:&Path,id:&str)->(ExtensionContext,Arc<std::sync::Mutex<Vec<String>>>) {let mut ctx=context(home,id);let notifications=Arc::default();ctx.ui=Arc::new(Ui {notifications:Arc::clone(&notifications)});(ctx,notifications)}
pub fn api()->ExtensionApi {ExtensionApi::new(LoadedExtension::new("telemetry",PathBuf::from("/workspace"),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),ExtensionRuntime::default())}
pub async fn dispatch(api:&ExtensionApi,mut event:ExtensionEvent,ctx:&ExtensionContext) {if let Some(handlers)=api.registered.handlers.get(&event.kind()) {for handler in handlers {(handler)(&mut event,ctx).await.unwrap();}}}
pub fn env(home:&Path)->telemetry_core::TelemetryEnv {BTreeMap::from([("POSTHOG_API_KEY".into(),"fixture-key".into()),("SENPI_CODING_AGENT_DIR".into(),home.to_string_lossy().into_owned())]).into_iter().collect()}
