use maho_ext_api::*;
use maho_ext_pi_goal::goal::{tool_registration::*,types::*,store::*};
use std::{path::{Path,PathBuf},sync::{Arc,Mutex}};
struct Session;
impl ToolSessionManager for Session{fn session_id(&self)->&str{"fixture"}fn session_file(&self)->Option<&Path>{None}}
impl SessionManager for Session{fn get_entries(&self)->Vec<SessionEntry>{Vec::new()}fn get_branch(&self)->Vec<SessionEntry>{Vec::new()}fn get_leaf_id(&self)->Option<String>{None}fn get_session_name(&self)->Option<String>{None}}
struct Registry;
impl ModelRegistry for Registry{fn get_all(&self)->Vec<Model>{Vec::new()}fn get_available(&self)->Vec<Model>{Vec::new()}fn find(&self,_:&str,_:&str)->Option<Model>{None}fn has_configured_auth(&self,_:&Model)->bool{false}fn get_api_key_for_provider<'a>(&'a self,_:&'a str)->ExtensionFuture<'a,Option<String>>{Box::pin(async{Ok(None)})}}
struct Ui;
impl ExtensionUi for Ui{
    fn select<'a>(&'a self,_:&'a str,_:&'a [String],_:ExtensionUiDialogOptions)->UiFuture<'a,Option<String>>{Box::pin(async{None})}
    fn confirm<'a>(&'a self,_:&'a str,_:&'a str,_:ExtensionUiDialogOptions)->UiFuture<'a,bool>{Box::pin(async{false})}
    fn input<'a>(&'a self,_:&'a str,_:Option<&'a str>,_:ExtensionUiDialogOptions)->UiFuture<'a,Option<String>>{Box::pin(async{None})}
    fn notify(&self,_:&str,_:NotificationType){}fn set_status(&self,_:&str,_:Option<&str>){}fn set_widget(&self,_:&str,_:Option<WidgetContent>,_:ExtensionWidgetOptions){}fn set_header(&self,_:Option<ComponentFactory>){}fn set_footer(&self,_:Option<ComponentFactory>){}fn set_title(&self,_:&str){}fn paste_to_editor(&self,_:&str){}fn set_editor_text(&self,_:&str){}fn get_editor_text(&self)->String{String::new()}
    fn custom(&self,_:ComponentFactory,_:CustomUiOptions)->ExtensionFuture<'_,JsonValue>{Box::pin(async{Err("headless fixture".into())})}fn theme(&self)->Theme{Theme::default()}
}
fn context(cwd:&Path)->ExtensionContext{ExtensionContext{ui:Arc::new(Ui),mode:ExtensionMode::Print,has_ui:false,cwd:cwd.into(),agent_dir:cwd.join("agent"),session_manager:Arc::new(Session),model_registry:Arc::new(Registry),model:None,thinking_level:None,service_tier:None,effective_service_tier:None,scoped_models:Vec::new(),goal_store_file:None,loaded_extension_paths:Vec::new(),signal:None,steering_signal:None,is_idle_fn:Arc::new(||true),wait_for_idle_fn:Arc::new(||Box::pin(async{})),is_project_trusted_fn:Arc::new(||true),is_compacting_fn:Arc::new(||false),get_system_prompt_fn:Arc::new(String::new),get_system_prompt_options_fn:Arc::new(BuildSystemPromptOptions::default),registered_mcp_servers:Vec::new(),update_tool_hook_status:None}}
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
