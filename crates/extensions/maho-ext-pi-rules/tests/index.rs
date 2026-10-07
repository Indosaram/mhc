use maho_ext_api::*;
use std::{path::Path,sync::Arc};
#[test]fn default_rules_factory_registers_existing_hooks_and_commands(){let mut api=ExtensionApi::new(LoadedExtension::new("pi-rules",Path::new(".").into(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),ExtensionRuntime::default());Extension::register(&maho_ext_pi_rules::RulesExtension,&mut api);for kind in [EventKind::SessionStart,EventKind::SessionCompact,EventKind::BeforeAgentStart,EventKind::ToolResult]{assert_eq!(api.registered.handlers[&kind].len(),1);}}
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
fn context(cwd:&Path)->ExtensionContext{ExtensionContext{ui:Arc::new(Ui),mode:ExtensionMode::Print,has_ui:false,cwd:cwd.into(),agent_dir:cwd.join("agent"),session_manager:Arc::new(Session),model_registry:Arc::new(Registry),model:None,thinking_level:None,service_tier:None,effective_service_tier:None,scoped_models:Vec::new(),goal_store_file:None,loaded_extension_paths:Vec::new(),signal:None,steering_signal:None,is_idle_fn:Arc::new(||true),wait_for_idle_fn:Arc::new(||Box::pin(async{})),is_project_trusted_fn:Arc::new(||true),is_compacting_fn:Arc::new(||false),get_system_prompt_fn:Arc::new(String::new),get_system_prompt_options_fn:Arc::new(BuildSystemPromptOptions::default),registered_mcp_servers:Vec::new(),update_tool_hook_status: None, idle_coordinator: None, logger: None, defer_macrotask: None, compaction_signal: Default::default()}}
struct FixtureFilesystem{home:std::path::PathBuf}
#[derive(Default)]
struct ScanActions(std::sync::Mutex<Vec<(String,Option<JsonValue>)>>);
impl ExtensionActions for ScanActions {
    fn send_message(&self,_:CustomMessage,_:SendMessageOptions)->Result<(),ExtensionFailure>{Ok(())}
    fn send_user_message(&self,_:UserMessageContent,_:SendUserMessageOptions)->Result<(),ExtensionFailure>{Ok(())}
    fn append_entry(&self,kind:&str,data:Option<JsonValue>)->Result<(),ExtensionFailure>{self.0.lock().expect("entries").push((kind.into(),data));Ok(())}
    fn get_all_tools(&self)->Result<Vec<ToolInfo>,ExtensionFailure>{Ok(Vec::new())}
}
impl maho_ext_pi_rules::rules::engine::EngineDeps for FixtureFilesystem{
    fn find_candidates(&mut self,options:maho_ext_pi_rules::rules::finder::FinderOptions<'_>)->Vec<maho_ext_pi_rules::rules::types::RuleCandidate>{maho_ext_pi_rules::rules::finder::find_rule_candidates(maho_ext_pi_rules::rules::finder::FinderOptions{project_root:options.project_root,target_file:options.target_file,home_dir:Some(&self.home),disabled_sources:options.disabled_sources,skip_user_home:options.skip_user_home,cache:options.cache})}
    fn read_file(&mut self,path:&str)->Option<String>{std::fs::read_to_string(path).ok()}
    fn find_project_root(&mut self,path:&str)->Option<String>{maho_ext_pi_rules::rules::project_root::find_project_root(Path::new(path),None).map(|path|path.to_string_lossy().into_owned())}
}
fn register_fixture(api:&mut ExtensionApi,root:&Path){api.runtime.bind(Arc::new(ScanActions::default()));maho_ext_pi_rules::index::register_rule_injection_hooks_with_engine(api,maho_ext_pi_rules::rules::engine::Engine::new(maho_ext_pi_rules::config::config_from_values(|_|None),FixtureFilesystem{home:root.join("fixture-home")}));}
#[tokio::test]
async fn hook_discovers_only_its_injected_home_rules(){
    let first=tempfile::tempdir().expect("first");let second=tempfile::tempdir().expect("second");
    for (root,body) in [(first.path(),"first isolated home rule"),(second.path(),"second isolated home rule")]{std::fs::create_dir(root.join(".git")).expect("marker");std::fs::create_dir_all(root.join("fixture-home/.omo/rules")).expect("home rules");std::fs::write(root.join("fixture-home/.omo/rules/global.md"),format!("---\nalwaysApply: true\n---\n{body}")).expect("rule");}
    for (root,expected,excluded) in [(first.path(),"first isolated home rule","second isolated home rule"),(second.path(),"second isolated home rule","first isolated home rule")]{
        let ctx=context(root);let mut api=ExtensionApi::new(LoadedExtension::new("pi-rules",root.into(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),ExtensionRuntime::default());register_fixture(&mut api,root);
        let mut event=ExtensionEvent::BeforeAgentStart(BeforeAgentStartEvent{prompt:"test".into(),images:None,system_prompt:String::new(),system_prompt_options:BuildSystemPromptOptions::default()});
        let result=api.registered.handlers[&EventKind::BeforeAgentStart][0](&mut event,&ctx).await.expect("hook");let EventResult::BeforeAgentStart(result)=result else{panic!("injection")};let prompt=result.system_prompt.expect("prompt");assert!(prompt.contains(expected));assert!(!prompt.contains(excluded));
    }
}
#[tokio::test]
async fn session_scan_entries_follow_current_runtime_binding_and_disabled_start() {
    let root=tempfile::tempdir().expect("fixture");let ctx=context(root.path());
    let mut api=ExtensionApi::new(LoadedExtension::new("pi-rules",root.path().into(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),ExtensionRuntime::default());
    register_fixture(&mut api,root.path());
    let first=Arc::new(ScanActions::default());api.runtime.bind(first.clone());
    let mut event=ExtensionEvent::SessionStart(SessionStartEvent{reason:SessionReason::New,initial_model_provenance:None,previous_session_file:None});
    let hook=&api.registered.handlers[&EventKind::SessionStart][0];hook(&mut event,&ctx).await.expect("start");
    assert_eq!(*first.0.lock().expect("entries"),vec![("pi-rules.scan".into(),Some(serde_json::json!({"cwd":root.path(),"reason":"new"})))]);
    let second=Arc::new(ScanActions::default());api.runtime.bind(second.clone());
    event=ExtensionEvent::SessionStart(SessionStartEvent{reason:SessionReason::Fork,initial_model_provenance:None,previous_session_file:None});
    hook(&mut event,&ctx).await.expect("fork");
    assert_eq!(first.0.lock().expect("entries").len(),1);
    assert_eq!(second.0.lock().expect("entries")[0].1,Some(serde_json::json!({"cwd":root.path(),"reason":"fork"})));
    api.set_flag("pi-rules-disabled",FlagValue::Boolean(true));hook(&mut event,&ctx).await.expect("disabled start");
    assert_eq!(second.0.lock().expect("entries").len(),1);
    let mut compact=ExtensionEvent::SessionCompact(SessionCompactEvent::Rejected{reason:CompactionReason::Manual,request_id:"fixture".into(),rejection_cause:CompactionRejectionCause::CancelledByExtension});
    api.registered.handlers[&EventKind::SessionCompact][0](&mut compact,&ctx).await.expect("compact");
    assert_eq!(second.0.lock().expect("entries")[1].1,Some(serde_json::json!({"cwd":root.path(),"reason":"compact"})));
}
#[test]
fn registers_four_injection_hooks_and_presence_flags(){
    let mut api=ExtensionApi::new(LoadedExtension::new("pi-rules","/fixture".into(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),ExtensionRuntime::default());
    register_fixture(&mut api,Path::new("/fixture"));
    for kind in [EventKind::SessionStart,EventKind::SessionCompact,EventKind::BeforeAgentStart,EventKind::ToolResult]{assert_eq!(api.registered.handlers[&kind].len(),1);}
    assert_eq!(api.registered.handlers.len(),4);assert_eq!(api.registered.flags.len(),2);
    assert_eq!(api.registered.commands.len(),2);assert!(api.registered.commands.iter().any(|command|command.name=="rules"));assert!(api.registered.commands.iter().any(|command|command.name=="reload-rules"));
}
#[tokio::test]
async fn registered_rules_command_completes_source_prefixes() {
    let root = tempfile::tempdir().expect("isolated root");
    let mut api = ExtensionApi::new(LoadedExtension::new("pi-rules", root.path().into(), SourceInfo::default()),
        ExtensionSessionProfile::default(), EventBus::default(), ExtensionRuntime::default());
    register_fixture(&mut api, root.path());
    let runner = maho_ext_host::runner::ExtensionRunner::new(
        vec![api.registered], api.runtime, api.events, context(root.path()));
    for (prefix, expected) in [("", vec!["list", "show", "paths", "status"]), ("s", vec!["show", "status"]), ("paths", vec!["paths"])] {
        let items = runner.get_command_argument_completions("rules", prefix).await.expect("completion result").expect("matching items");
        assert_eq!(items.iter().map(|item| item.value.as_str()).collect::<Vec<_>>(), expected);
        assert!(items.iter().all(|item| item.label == item.value && item.description.is_none()));
    }
    assert!(runner.get_command_argument_completions("rules", "missing").await.expect("unmatched prefix").is_none());
    assert!(runner.get_command_argument_completions("reload-rules", "").await.expect("reload command").is_none());
}
#[tokio::test]
async fn static_hook_is_immutable_deduplicated_and_session_resettable(){
    let temp=tempfile::tempdir().expect("temp");std::fs::create_dir(temp.path().join(".git")).expect("project marker");std::fs::write(temp.path().join("AGENTS.md"),"fixture rule").expect("rule");let ctx=context(temp.path());let mut api=ExtensionApi::new(LoadedExtension::new("pi-rules",temp.path().into(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),ExtensionRuntime::default());register_fixture(&mut api,temp.path());
    let mut event=ExtensionEvent::BeforeAgentStart(BeforeAgentStartEvent{prompt:"test".into(),images:None,system_prompt:"original".into(),system_prompt_options:BuildSystemPromptOptions::default()});let hook=&api.registered.handlers[&EventKind::BeforeAgentStart][0];let result=hook(&mut event,&ctx).await.expect("first");let EventResult::BeforeAgentStart(result)=result else{panic!("injection")};assert!(result.system_prompt.expect("prompt").starts_with("original"));let ExtensionEvent::BeforeAgentStart(original)=&event else{panic!("event")};assert_eq!(original.system_prompt,"original");assert!(matches!(hook(&mut event,&ctx).await.expect("dedup"),EventResult::None));
    let mut reset=ExtensionEvent::SessionStart(SessionStartEvent{reason:SessionReason::New,initial_model_provenance:None,previous_session_file:None});api.registered.handlers[&EventKind::SessionStart][0](&mut reset,&ctx).await.expect("reset");assert!(matches!(hook(&mut event,&ctx).await.expect("after reset"),EventResult::BeforeAgentStart(_)));
    (api.registered.commands.iter().find(|command|command.name=="reload-rules").expect("reload registration").handler)("",&ctx).await.expect("reload command");assert!(matches!(hook(&mut event,&ctx).await.expect("after reload"),EventResult::BeforeAgentStart(_)));
}
#[tokio::test]
async fn dynamic_hook_preserves_content_and_deduplicates_target_fingerprint(){
    let temp=tempfile::tempdir().expect("temp");std::fs::create_dir(temp.path().join(".git")).expect("project marker");std::fs::create_dir_all(temp.path().join(".omo/rules")).expect("rule directory");std::fs::write(temp.path().join(".omo/rules/dynamic.md"),"---\nglobs: \"**/*.rs\"\n---\nfixture dynamic rule").expect("rule");std::fs::write(temp.path().join("sample.rs"),"fn main() {}").expect("target");let ctx=context(temp.path());let mut api=ExtensionApi::new(LoadedExtension::new("pi-rules",temp.path().into(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),ExtensionRuntime::default());register_fixture(&mut api,temp.path());
    let original=vec![ToolContent::text("original")];let mut event=ExtensionEvent::ToolResult(ToolResultEvent{tool_call_id:"read-fixture".into(),tool_name:"read".into(),input:serde_json::json!({"path":"sample.rs"}),content:original.clone(),details:None,is_error:false,usage:None});let hook=&api.registered.handlers[&EventKind::ToolResult][0];let result=hook(&mut event,&ctx).await.expect("injection");let EventResult::ToolResult(result)=result else{panic!("tool result")};let content=result.content.expect("content");assert_eq!(&content[..original.len()],original.as_slice());assert_eq!(content.len(),original.len()+1);let ExtensionEvent::ToolResult(event_original)=&event else{panic!("event")};assert_eq!(event_original.content,original);assert!(matches!(hook(&mut event,&ctx).await.expect("dedup"),EventResult::None));
}

#[tokio::test]
async fn dynamic_hook_reinjects_changed_rule_without_cross_session_state() {
    let root = tempfile::tempdir().expect("fixture");
    std::fs::create_dir(root.path().join(".git")).expect("marker");
    std::fs::create_dir_all(root.path().join(".omo/rules")).expect("rules");
    let rule = root.path().join(".omo/rules/dynamic.md");
    std::fs::write(&rule, "---\nglobs: '**/*.rs'\n---\nfirst body").expect("rule");
    std::fs::write(root.path().join("sample.rs"), "fn main() {}").expect("target");
    let ctx = context(root.path());
    let mut api = ExtensionApi::new(LoadedExtension::new("pi-rules", root.path().into(), Default::default()), Default::default(), Default::default(), Default::default());
    register_fixture(&mut api, root.path());
    let mut event = ExtensionEvent::ToolResult(ToolResultEvent { tool_call_id: "qa".into(), tool_name: "read".into(), input: serde_json::json!({"path":"sample.rs"}), content: Vec::new(), details: None, is_error: false, usage: None });
    let hook = &api.registered.handlers[&EventKind::ToolResult][0];
    assert!(matches!(hook(&mut event, &ctx).await.expect("initial"), EventResult::ToolResult(_)));
    assert!(matches!(hook(&mut event, &ctx).await.expect("unchanged"), EventResult::None));
    std::fs::write(&rule, "---\nglobs: '**/*.rs'\n---\nchanged body with different length").expect("changed rule");
    std::fs::write(root.path().join("sample.rs"), "fn main() { println!(\"changed\"); }").expect("changed target");
    let EventResult::ToolResult(result) = hook(&mut event, &ctx).await.expect("changed injection") else { panic!("reinjection"); };
    let text = result.content.expect("content").into_iter().find_map(|content| match content { ToolContent::Text { text, .. } => Some(text), _ => None }).expect("text");
    assert!(text.contains("changed body with different length"));
    assert!(!text.contains("first body"));
    let mut reset = ExtensionEvent::SessionStart(SessionStartEvent { reason: SessionReason::New, initial_model_provenance: None, previous_session_file: None });
    api.registered.handlers[&EventKind::SessionStart][0](&mut reset, &ctx).await.expect("session reset");
    assert!(matches!(hook(&mut event, &ctx).await.expect("new session"), EventResult::ToolResult(_)));
}

#[tokio::test]
async fn dynamic_hook_matches_structural_grammar_pattern(){
    let temp=tempfile::tempdir().expect("temp");std::fs::create_dir(temp.path().join(".git")).expect("project marker");std::fs::create_dir_all(temp.path().join(".omo/rules")).expect("rule directory");std::fs::write(temp.path().join(".omo/rules/grammar.md"),"---\nglobs: '**/*.{rs,ts}'\n---\nfixture grammar rule").expect("rule");std::fs::write(temp.path().join("sample.rs"),"fn main() {}").expect("target");
    let ctx=context(temp.path());let mut api=ExtensionApi::new(LoadedExtension::new("pi-rules",temp.path().into(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),ExtensionRuntime::default());register_fixture(&mut api,temp.path());
    let mut event=ExtensionEvent::ToolResult(ToolResultEvent{tool_call_id:"grammar".into(),tool_name:"read".into(),input:serde_json::json!({"path":"sample.rs"}),content:Vec::new(),details:None,is_error:false,usage:None});
    let hook=&api.registered.handlers[&EventKind::ToolResult][0];
    let EventResult::ToolResult(result)=hook(&mut event,&ctx).await.expect("grammar injection")else{panic!("tool result")};
    let text=result.content.expect("content").into_iter().find_map(|content|match content{ToolContent::Text{text,..}=>Some(text),_=>None}).expect("text");
    assert!(text.contains("fixture grammar rule"),"{text}");
}
