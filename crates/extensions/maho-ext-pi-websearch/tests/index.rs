use maho_ext_api::*;
use maho_ext_pi_websearch::index::*;
use std::{path::Path,sync::{Arc,Mutex}};
use maho_ext_pi_websearch::websearch::types::ConfigLoadResult;
struct Session;
impl ToolSessionManager for Session{fn session_id(&self)->&str{"fixture"}fn session_file(&self)->Option<&Path>{None}}
impl SessionManager for Session{fn get_entries(&self)->Vec<SessionEntry>{Vec::new()}fn get_branch(&self)->Vec<SessionEntry>{Vec::new()}fn get_leaf_id(&self)->Option<String>{None}fn get_session_name(&self)->Option<String>{None}}
struct Registry;
impl ModelRegistry for Registry{fn get_all(&self)->Vec<Model>{Vec::new()}fn get_available(&self)->Vec<Model>{Vec::new()}fn find(&self,_:&str,_:&str)->Option<Model>{None}fn has_configured_auth(&self,_:&Model)->bool{false}fn get_api_key_for_provider<'a>(&'a self,_:&'a str)->ExtensionFuture<'a,Option<String>>{Box::pin(async{Ok(None)})}}
struct Ui{state:Arc<Mutex<ConfigLoadResult>>,observed:Mutex<Vec<bool>>}
impl ExtensionUi for Ui{
    fn select<'a>(&'a self,_:&'a str,_:&'a [String],_:ExtensionUiDialogOptions)->UiFuture<'a,Option<String>>{Box::pin(async{None})}
    fn confirm<'a>(&'a self,_:&'a str,_:&'a str,_:ExtensionUiDialogOptions)->UiFuture<'a,bool>{Box::pin(async{false})}
    fn input<'a>(&'a self,_:&'a str,_:Option<&'a str>,_:ExtensionUiDialogOptions)->UiFuture<'a,Option<String>>{Box::pin(async{None})}
    fn notify(&self,_:&str,_:NotificationType){}fn set_status(&self,_:&str,_:Option<&str>){self.observed.lock().expect("observations").push(matches!(*self.state.lock().expect("state"),ConfigLoadResult::Success{..}));}fn set_widget(&self,_:&str,_:Option<WidgetContent>,_:ExtensionWidgetOptions){}fn set_header(&self,_:Option<ComponentFactory>){}fn set_footer(&self,_:Option<ComponentFactory>){}fn set_title(&self,_:&str){}fn paste_to_editor(&self,_:&str){}fn set_editor_text(&self,_:&str){}fn get_editor_text(&self)->String{String::new()}
    fn custom(&self,_:ComponentFactory,_:CustomUiOptions)->ExtensionFuture<'_,JsonValue>{Box::pin(async{Err("headless fixture".into())})}fn theme(&self)->Theme{Theme::default()}
}
#[tokio::test]async fn session_publishes_loaded_config_before_ui_callbacks(){
    let temp=tempfile::tempdir().expect("temp");std::fs::create_dir(temp.path().join(".pi")).expect("config directory");std::fs::write(temp.path().join(".pi/websearch.json"),r#"{"providers":[{"provider":"duckduckgo-html"}],"auto":false}"#).expect("config");
    let mut api=ExtensionApi::new(LoadedExtension::new("pi-websearch",temp.path().into(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),ExtensionRuntime::default());let state=register_search_lifecycle(&mut api);let ui=Arc::new(Ui{state:Arc::clone(&state),observed:Mutex::new(Vec::new())});
    let ctx=ExtensionContext{ui:ui.clone(),mode:ExtensionMode::Print,has_ui:true,cwd:temp.path().into(),agent_dir:temp.path().join("agent"),session_manager:Arc::new(Session),model_registry:Arc::new(Registry),model:None,thinking_level:None,service_tier:None,effective_service_tier:None,scoped_models:Vec::new(),goal_store_file:None,loaded_extension_paths:Vec::new(),signal:None,steering_signal:None,is_idle_fn:Arc::new(||true),wait_for_idle_fn:Arc::new(||Box::pin(async{})),is_project_trusted_fn:Arc::new(||true),is_compacting_fn:Arc::new(||false),get_system_prompt_fn:Arc::new(String::new),get_system_prompt_options_fn:Arc::new(BuildSystemPromptOptions::default),registered_mcp_servers:Vec::new(),update_tool_hook_status:None};
    let mut event=ExtensionEvent::SessionStart(SessionStartEvent{reason:SessionReason::New,initial_model_provenance:None,previous_session_file:None});api.registered.handlers[&EventKind::SessionStart][0](&mut event,&ctx).await.expect("start");assert_eq!(*ui.observed.lock().expect("observations"),vec![true]);
}
#[test]fn bypass_matches_only_native_provider_extensions(){assert!(is_provider_native_bypass(Some("openai")));assert!(is_provider_native_bypass(Some("anthropic")));for provider in [None,Some("openrouter"),Some("custom"),Some("OpenAI")]{assert!(!is_provider_native_bypass(provider));}}
#[test]fn registers_lifecycle_and_status_command(){let mut api=ExtensionApi::new(LoadedExtension::new("pi-websearch","/fixture".into(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),ExtensionRuntime::default());register_search_lifecycle(&mut api);assert_eq!(api.registered.handlers[&EventKind::SessionStart].len(),1);assert_eq!(api.registered.handlers[&EventKind::SessionShutdown].len(),1);assert!(api.registered.commands.iter().any(|command|command.name=="websearch"));}
