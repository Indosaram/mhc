//! Terminal persistence lifecycle binding (upstream `adoptPersistedTerminalState` +
//! `suspendAndFlushManifest`): the extension acquires the session lease from the host-exposed
//! session dir on a non-reload start, restores the manifest while it owns the lease, and
//! records + releases on shutdown.
use maho_ext_api::*;
use std::{path::{Path,PathBuf},sync::Arc};

struct Session { dir: PathBuf }
impl ToolSessionManager for Session {
    fn session_id(&self)->&str {"terminal-lifecycle"}
    fn session_file(&self)->Option<&Path> {None}
}
impl SessionManager for Session {
    fn get_entries(&self)->Vec<SessionEntry> {vec![]}
    fn get_branch(&self)->Vec<SessionEntry> {vec![]}
    fn get_leaf_id(&self)->Option<String> {None}
    fn get_session_name(&self)->Option<String> {None}
    fn get_session_dir(&self)->Option<PathBuf> {Some(self.dir.clone())}
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
    ExtensionContext {ui:Arc::new(Ui),mode:ExtensionMode::Print,has_ui:false,cwd:dir.into(),agent_dir:dir.into(),session_manager:Arc::new(Session {dir:dir.into()}),model_registry:Arc::new(Registry),model:None,thinking_level:None,service_tier:None,effective_service_tier:None,scoped_models:vec![],goal_store_file:None,loaded_extension_paths:vec![],signal:None,steering_signal:None,is_idle_fn:Arc::new(||true),wait_for_idle_fn:Arc::new(||Box::pin(async {})),is_project_trusted_fn:Arc::new(||true),is_compacting_fn:Arc::new(||false),get_system_prompt_fn:Arc::new(String::new),get_system_prompt_options_fn:Arc::new(BuildSystemPromptOptions::default),registered_mcp_servers:vec![],update_tool_hook_status:None,idle_coordinator:None,logger:None,defer_macrotask:None}
}
fn api(dir:&Path)->ExtensionApi {
    let mut api=ExtensionApi::new(LoadedExtension::new("terminal",dir.into(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),ExtensionRuntime::default());maho_ext_terminal::extension::TerminalExtension.register(&mut api);api
}
async fn start(api:&ExtensionApi,ctx:&ExtensionContext,reason:SessionReason) {
    let mut event=ExtensionEvent::SessionStart(SessionStartEvent {reason,initial_model_provenance:None,previous_session_file:None});
    for handler in &api.registered.handlers[&EventKind::SessionStart] {handler(&mut event,ctx).await.expect("session start handler");}
}
async fn shutdown(api:&ExtensionApi,ctx:&ExtensionContext,reason:SessionReason) {
    let mut event=ExtensionEvent::SessionShutdown(SessionShutdownEvent {reason,target_session_file:None,signal:None});
    for handler in &api.registered.handlers[&EventKind::SessionShutdown] {handler(&mut event,ctx).await.expect("shutdown handler");}
}
fn encoded()->String {maho_core::session_sidecar_store::encoded_session_id("terminal-lifecycle")}
fn state_dir(dir:&Path)->PathBuf {dir.join("extensions/terminal")}

#[tokio::test]
async fn session_start_acquires_the_lease_and_shutdown_records_then_releases_it() {
    let dir=tempfile::tempdir().unwrap();let ctx=context(dir.path());
    let lease=state_dir(dir.path()).join(format!("{}.lease",encoded()));
    let manifest=state_dir(dir.path()).join(format!("{}.json",encoded()));
    let api=api(dir.path());
    start(&api,&ctx,SessionReason::Startup).await;
    assert!(lease.is_file(),"the lease must be acquired from the session dir");
    assert!(!manifest.exists(),"an empty manifest is not written before shutdown");
    shutdown(&api,&ctx,SessionReason::Quit).await;
    assert!(!lease.exists(),"shutdown releases the lease");
    assert!(manifest.is_file(),"shutdown records the manifest");
}

#[tokio::test]
async fn a_reload_generation_inherits_the_lease_and_records_and_releases_on_quit() {
    let dir=tempfile::tempdir().unwrap();let ctx=context(dir.path());
    let lease=state_dir(dir.path()).join(format!("{}.lease",encoded()));
    let manifest=state_dir(dir.path()).join(format!("{}.json",encoded()));
    let owner=api(dir.path());
    start(&owner,&ctx,SessionReason::Startup).await;
    assert!(lease.is_file());
    shutdown(&owner,&ctx,SessionReason::Reload).await;
    assert!(lease.is_file(),"a reload keeps the lease");
    assert!(!manifest.exists(),"a reload records nothing");
    let reloaded=api(dir.path());
    start(&reloaded,&ctx,SessionReason::Reload).await;
    assert!(lease.is_file());
    shutdown(&reloaded,&ctx,SessionReason::Quit).await;
    assert!(!lease.exists(),"the inherited lease is released by (path, own pid)");
    assert!(manifest.is_file(),"the reload generation records the manifest");
}

#[tokio::test]
async fn a_context_without_a_session_dir_keeps_no_durable_terminal_state() {
    let dir=tempfile::tempdir().unwrap();
    let mut ctx=context(dir.path());
    ctx.session_manager=Arc::new(Session {dir:PathBuf::new()});
    let api=api(dir.path());
    start(&api,&ctx,SessionReason::Startup).await;
    shutdown(&api,&ctx,SessionReason::Quit).await;
    assert!(!state_dir(dir.path()).exists(),"no session dir means no lease and no manifest");
}
