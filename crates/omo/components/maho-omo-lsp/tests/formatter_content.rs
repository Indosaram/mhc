use std::{path::{Path, PathBuf}, sync::{Arc, Mutex}};
use maho_ext_api::*;
use maho_omo_formatter::{create_formatter_step, DaemonFormatResult, FormatMode, FormatOnMutationConfig, FormatterStepOptions};
use maho_omo_lsp::index::{LspComponent, LspComponentOptions};
use lsp_core::post_edit::PostEditDiagnosticsOutcome;
use serde_json::json;

struct Session;
impl ToolSessionManager for Session {
    fn session_id(&self)->&str {"format-session"}
    fn session_file(&self)->Option<&Path> {None}
}
impl SessionManager for Session {
    fn get_entries(&self)->Vec<SessionEntry> {Vec::new()}
    fn get_branch(&self)->Vec<SessionEntry> {Vec::new()}
    fn get_leaf_id(&self)->Option<String> {None}
    fn get_session_name(&self)->Option<String> {None}
}
struct Registry;
impl ModelRegistry for Registry {
    fn get_all(&self)->Vec<Model> {Vec::new()}
    fn get_available(&self)->Vec<Model> {Vec::new()}
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
    fn custom(&self,_:ComponentFactory,_:CustomUiOptions)->ExtensionFuture<'_,JsonValue> {Box::pin(async {Err(ExtensionFailure::new("headless"))})}
    fn theme(&self)->Theme {Theme::default()}
}
fn context(cwd:PathBuf)->ExtensionContext {
    ExtensionContext {
        ui:Arc::new(Ui),mode:ExtensionMode::Print,has_ui:false,cwd:cwd.clone(),agent_dir:cwd.join("agent"),session_manager:Arc::new(Session),model_registry:Arc::new(Registry),
        model:None,thinking_level:None,service_tier:None,effective_service_tier:None,scoped_models:Vec::new(),goal_store_file:None,loaded_extension_paths:Vec::new(),signal:None,steering_signal:None,
        is_idle_fn:Arc::new(||true),wait_for_idle_fn:Arc::new(||Box::pin(async {})),is_project_trusted_fn:Arc::new(||true),is_compacting_fn:Arc::new(||false),get_system_prompt_fn:Arc::new(String::new),get_system_prompt_options_fn:Arc::new(BuildSystemPromptOptions::default),
        registered_mcp_servers:Vec::new(),update_tool_hook_status:None,idle_coordinator:None,logger:None,defer_macrotask:None,compaction_signal:Default::default(),
    }
}
async fn content_case(tool:&str,diagnostics:&str,formatted:bool) {
    // Given: real native formatter and LSP hook, with only daemon outcomes supplied.
    let root=tempfile::tempdir().expect("private mutation workspace");
    let path=root.path().join("sample.ts");
    std::fs::write(&path,"const value=1\n").expect("mutation output");
    let calls=Arc::new(Mutex::new(Vec::new()));
    let format_calls=Arc::clone(&calls);
    let formatter=create_formatter_step(FormatterStepOptions {
        markers:Some(Box::new(|_|vec!["biome.json".into()])),
        daemon_format:Some(Box::new(move |path| {
            format_calls.lock().expect("call order").push(("format",path.to_string_lossy().into_owned()));
            Box::pin(async move {Ok(DaemonFormatResult {details:Some(json!({"status":if formatted {"formatted"} else {"unchanged"},"linesAdded":1,"linesRemoved":1})),..Default::default()})})
        })),..Default::default()
    });
    let diagnostic_calls=Arc::clone(&calls);
    let has_errors=diagnostics=="errors";
    let runtime=ExtensionRuntime::default();
    runtime.set_flag("omo-senpi-lsp-post-edit-diagnostics-enabled",FlagValue::Boolean(diagnostics!="disabled"));
    let mut api=ExtensionApi::new(LoadedExtension::new("lsp",root.path().to_path_buf(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),runtime);
    LspComponent.register_with_options(&mut api,LspComponentOptions {
        formatter:Some(formatter),run_diagnostics:Some(Arc::new(move |path| {
            diagnostic_calls.lock().expect("call order").push(("diagnose",path));
            Box::pin(async move {Ok(PostEditDiagnosticsOutcome::Text(if has_errors {"TS2322".into()} else {String::new()}))})
        })),
    });
    let original=ToolContent::text("mutation complete");
    let mut event=ExtensionEvent::ToolResult(ToolResultEvent {
        tool_call_id:"format-call".into(),tool_name:tool.into(),input:if tool=="apply_patch" {json!({"input":"*** Begin Patch\n*** Update File: sample.ts\n@@\n-const value=0\n+const value=1\n*** End Patch"})} else {json!({"path":path})},
        content:vec![original.clone()],details:None,is_error:false,usage:None,
    });
    // When: the registered production hook receives the mutation result.
    let handler=&api.registered.handlers[&EventKind::ToolResult][0];
    let result=handler(&mut event,&context(root.path().to_path_buf())).await.expect("hook result");
    // Then: format precedes diagnostics, and both additions survive composition.
    if formatted {
        let EventResult::ToolResult(result)=result else {panic!("expected content additions")};
        let content=result.content.expect("composed content");
        assert_eq!(content.len(),if has_errors {3} else {2});
        assert_eq!(content[0],original);
        assert!(content.iter().all(|block|matches!(block,ToolContent::Text {..})));
    } else {assert!(matches!(result,EventResult::None));}
    let calls=calls.lock().expect("call order");
    let mut expected=vec![("format",path.to_string_lossy().into_owned())];
    if diagnostics!="disabled" {expected.push(("diagnose",if tool=="apply_patch" {"sample.ts".into()} else {path.to_string_lossy().into_owned()}));}
    assert_eq!(*calls,expected);
    let ExtensionEvent::ToolResult(event)=event else {panic!("original event variant")};
    assert_eq!(event.content,vec![original]);
}
macro_rules! case {
    ($name:ident,$tool:literal,$diagnostics:literal)=> {
        #[tokio::test] async fn $name() {content_case($tool,$diagnostics,true).await;}
    };
}
case!(write_errors,"write","errors");
case!(write_clean,"write","clean");
case!(write_disabled,"write","disabled");
case!(edit_errors,"edit","errors");
case!(edit_clean,"edit","clean");
case!(edit_disabled,"edit","disabled");
case!(patch_errors,"apply_patch","errors");
case!(patch_clean,"apply_patch","clean");
case!(patch_disabled,"apply_patch","disabled");
#[tokio::test] async fn unchanged_clean_adds_nothing() {content_case("write","clean",false).await;}

#[tokio::test]
async fn required_formatter_failure_marks_result_and_stops_diagnostics() {
    let root=tempfile::tempdir().expect("private required-format workspace");
    let path=root.path().join("sample.ts");
    std::fs::write(&path,"const value=1\n").expect("mutation output");
    let formatter=create_formatter_step(FormatterStepOptions {
        config:Some(FormatOnMutationConfig {mode:Some(FormatMode::Required),..Default::default()}),
        markers:Some(Box::new(|_|vec!["biome.json".into()])),
        daemon_format:Some(Box::new(|_|Box::pin(async {Ok(DaemonFormatResult {details:Some(json!({"reason":"capability_not_advertised"})),..Default::default()})}))),
        resolve_binary:Some(Box::new(|_,_|Some("/nonexistent/required-formatter".into()))),
        ..Default::default()
    });
    let mut api=ExtensionApi::new(LoadedExtension::new("lsp",root.path().to_path_buf(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),ExtensionRuntime::default());
    LspComponent.register_with_options(&mut api,LspComponentOptions {
        formatter:Some(formatter),run_diagnostics:Some(Arc::new(|_|panic!("required formatting failure must stop diagnostics"))),
    });
    let original=ToolContent::text("mutation complete");
    let mut event=ExtensionEvent::ToolResult(ToolResultEvent {tool_call_id:"required-format".into(),tool_name:"write".into(),input:json!({"path":path}),content:vec![original.clone()],details:None,is_error:false,usage:None});
    let result=api.registered.handlers[&EventKind::ToolResult][0](&mut event,&context(root.path().to_path_buf())).await.expect("required formatter result");
    let EventResult::ToolResult(result)=result else {panic!("required failure result")};
    assert_eq!(result.is_error,Some(true));
    let content=result.content.expect("original content preserved on required failure");
    assert_eq!(content,vec![original]);
}
