use maho_ext_api::{ExtensionFailure,ToolInfo};
/// One shared definition registry: the service's catalog refresh, the extension's
/// `get_tool_definition`, and the session actions the runtime registrar installs into all read it,
/// so the MCP doc and its registered schema cannot come from two independent mocks.
#[derive(Default)]
struct Catalog { tools: std::sync::Mutex<Vec<ToolInfo>> }
impl Catalog {
    fn with(tools: Vec<ToolInfo>) -> Self { Self { tools: std::sync::Mutex::new(tools) } }
    fn record(&self, tool: ToolInfo) { self.tools.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(tool); }
}
impl maho_ext_api::ExtensionActions for Catalog {
    fn send_message(&self,_:maho_ext_api::CustomMessage,_:maho_ext_api::SendMessageOptions)->Result<(),ExtensionFailure>{panic!("not used")}
    fn send_user_message(&self,_:maho_ext_api::UserMessageContent,_:maho_ext_api::SendUserMessageOptions)->Result<(),ExtensionFailure>{panic!("not used")}
    fn append_entry(&self,_:&str,_:Option<serde_json::Value>)->Result<(),ExtensionFailure>{panic!("not used")}
    fn get_all_tools(&self)->Result<Vec<ToolInfo>,ExtensionFailure>{Ok(self.tools.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone())}
}
fn read_docs_tool()->ToolInfo { ToolInfo {name:"read_docs".into(),label:"Read documentation".into(),description:"Read documentation and API references".into(),parameters:serde_json::json!({"type":"object"}),prompt_guidelines:None,source_info:maho_ext_api::SourceInfo {path:"builtin:docs".into(),..Default::default()},exposure:maho_ext_api::ToolExposure::Search,search_text:None,search_keywords:vec!["documentation".into()],search_group:Some("docs".into()),allow_lazy_activation:true} }
/// The real registered definition an MCP server contributes: a resident tool with its own object
/// schema and default (`Direct`) exposure, exactly like `build_mcp_tool_definitions` installs.
fn mcp_docs_definition()->maho_tools::definition::ToolDefinition {
    maho_tools::definition::ToolDefinition::new("mcp_docs","Read documentation and API references",serde_json::json!({"type":"object","properties":{"library":{"type":"string"}},"required":["library"]}),std::sync::Arc::new(|_|Box::pin(async {Ok(maho_ext_api::ToolResult::text("unused"))})))
}
struct Search;
struct FedSearch;
impl maho_ext_api::Extension for FedSearch {
    fn register(&self,api:&mut maho_ext_api::ExtensionApi){
        let catalog=std::sync::Arc::new(Catalog::with(vec![read_docs_tool()]));
        let service=std::sync::Arc::new(tokio::sync::Mutex::new(maho_ext_tool_search::service::ToolSearchService::new(api.runtime.clone(),catalog.clone())));
        maho_ext_tool_search::index::ToolSearchExtension{actions:catalog.clone(),mcp_native_enabled:std::sync::Arc::new(||false)}.register_with_service(api,service.clone());
        api.on(maho_ext_api::EventKind::BeforeAgentStart,std::sync::Arc::new(move |_,_|{let service=service.clone();Box::pin(async move{
            service.lock().await.feed(vec![maho_ext_tool_search::engine::document::ToolSearchDocument{name:"mcp_docs".into(),label:"MCP docs".into(),aliases:vec![],description:Some("documentation".into()),search_text:None,keywords:vec![],source:maho_ext_tool_search::engine::document::ToolSearchSource::Mcp,group:"docs".into(),owner_label:"server".into(),registration_id:"mcp:docs".into()}],std::sync::Arc::new(|_|panic!("search must not activate matches")))?;
            Ok(maho_ext_api::EventResult::None)
        })}));
    }
}
impl maho_ext_api::Extension for Search {
    fn register(&self,api:&mut maho_ext_api::ExtensionApi){
        maho_ext_tool_search::index::ToolSearchExtension {actions:std::sync::Arc::new(Catalog::with(vec![read_docs_tool()])),mcp_native_enabled:std::sync::Arc::new(||false)}.register(api);
    }
}
#[tokio::test]
async fn factory_returned_service_feeds_same_registered_search_executor(){
    use maho_ai::providers::faux::{faux_assistant_message,faux_tool_call,FauxAssistantMessageOptions};
    use maho_test_support::{faux::FauxScript,faux_session::FauxSession};
    let session=FauxSession::new(FauxScript{name:"same-service".into(),prompt:"search".into(),responses:vec![]})
        .with_native_extension(maho_ext_host::loader::NativeExtensionFactory{path:"builtin:tool-search".into(),source_info:maho_ext_api::SourceInfo{source:"builtin".into(),..Default::default()},extension:Box::new(FedSearch)})
        .with_native_responses(vec![faux_assistant_message(faux_tool_call("tool_search",serde_json::from_value(serde_json::json!({"query":"documentation","source":"mcp"})).unwrap(),Some("fed-search")),FauxAssistantMessageOptions{stop_reason:Some(maho_ai::types::StopReason::ToolUse),..Default::default()}),faux_assistant_message("done",FauxAssistantMessageOptions::default())]);
    let result=tokio::time::timeout(std::time::Duration::from_secs(10),session.run_native()).await.unwrap().unwrap();
    let tool=result["messages"].as_array().unwrap().iter().find(|message|message["role"]=="toolResult").unwrap();assert_eq!(tool["isError"],false,"{tool}");assert_eq!(tool["details"]["matched"],serde_json::json!(["mcp_docs"]));
}
#[tokio::test]
async fn native_session_searches_live_catalog_without_activating_matches() {
    use maho_ai::providers::faux::{faux_assistant_message,faux_tool_call,FauxAssistantMessageOptions};
    use maho_test_support::{faux::FauxScript,faux_session::FauxSession};
    let session=FauxSession::new(FauxScript {name:"tool-search-native".into(),prompt:"search documentation".into(),responses:vec![]})
        .with_native_extension(maho_ext_host::loader::NativeExtensionFactory {path:"builtin:tool-search".into(),source_info:maho_ext_api::SourceInfo {source:"builtin".into(),..Default::default()},extension:Box::new(Search)})
        .with_native_responses(vec![faux_assistant_message(faux_tool_call("tool_search",serde_json::from_value(serde_json::json!({"query":"documentation","source":"extension","group":"docs"})).unwrap(),Some("search-call")),FauxAssistantMessageOptions {stop_reason:Some(maho_ai::types::StopReason::ToolUse),timestamp:Some(0),..Default::default()}),faux_assistant_message("done",FauxAssistantMessageOptions {timestamp:Some(0),..Default::default()})]);
    let result=tokio::time::timeout(std::time::Duration::from_secs(10),session.run_native()).await.unwrap().unwrap();
    let tool=result["messages"].as_array().unwrap().iter().find(|message|message["role"]=="toolResult").unwrap();
    assert_eq!(tool["isError"],false);assert_eq!(tool["details"]["matched"],serde_json::json!(["read_docs"]));assert_eq!(tool["details"]["query"],"documentation");
}

use maho_ext_api::{BuildSystemPromptOptions,ComponentFactory,CustomUiOptions,ExtensionContext,ExtensionEvent,ExtensionFuture,ExtensionRuntime,ExtensionUi,ExtensionUiDialogOptions,JsonValue,Model,ModelRegistry,NotificationType,SessionEntry,SessionManager,Theme,ToolSessionManager,UiFuture,WidgetContent,ExtensionWidgetOptions};
use std::path::Path;
struct TestSession;
impl ToolSessionManager for TestSession {
    fn session_id(&self)->&str{"session"}
    fn session_file(&self)->Option<&Path>{None}
}
impl SessionManager for TestSession {
    fn get_entries(&self)->Vec<SessionEntry>{Vec::new()}
    fn get_branch(&self)->Vec<SessionEntry>{Vec::new()}
    fn get_leaf_id(&self)->Option<String>{None}
    fn get_session_name(&self)->Option<String>{None}
}
struct TestRegistry;
impl ModelRegistry for TestRegistry {
    fn get_all(&self)->Vec<Model>{Vec::new()}
    fn get_available(&self)->Vec<Model>{Vec::new()}
    fn find(&self,_:&str,_:&str)->Option<Model>{None}
    fn has_configured_auth(&self,_:&Model)->bool{false}
    fn get_api_key_for_provider<'a>(&'a self,_:&'a str)->ExtensionFuture<'a,Option<String>>{Box::pin(async{Ok(None)})}
}
struct TestUi;
impl ExtensionUi for TestUi {
    fn select<'a>(&'a self,_:&'a str,_:&'a [String],_:ExtensionUiDialogOptions)->UiFuture<'a,Option<String>>{Box::pin(async{None})}
    fn confirm<'a>(&'a self,_:&'a str,_:&'a str,_:ExtensionUiDialogOptions)->UiFuture<'a,bool>{Box::pin(async{false})}
    fn input<'a>(&'a self,_:&'a str,_:Option<&'a str>,_:ExtensionUiDialogOptions)->UiFuture<'a,Option<String>>{Box::pin(async{None})}
    fn notify(&self,_:&str,_:NotificationType){}
    fn set_status(&self,_:&str,_:Option<&str>){}
    fn set_widget(&self,_:&str,_:Option<WidgetContent>,_:ExtensionWidgetOptions){}
    fn set_header(&self,_:Option<ComponentFactory>){}
    fn set_footer(&self,_:Option<ComponentFactory>){}
    fn set_title(&self,_:&str){}
    fn paste_to_editor(&self,_:&str){}
    fn set_editor_text(&self,_:&str){}
    fn get_editor_text(&self)->String{String::new()}
    fn custom(&self,_:ComponentFactory,_:CustomUiOptions)->ExtensionFuture<'_,JsonValue>{Box::pin(async{Ok(serde_json::Value::Null)})}
    fn theme(&self)->Theme{Theme::default()}
}
fn context()->ExtensionContext {
    ExtensionContext { ui:std::sync::Arc::new(TestUi),mode:maho_ext_api::ExtensionMode::Print,has_ui:false,cwd:"/tmp".into(),agent_dir:"/tmp/agent".into(),
        session_manager:std::sync::Arc::new(TestSession),model_registry:std::sync::Arc::new(TestRegistry),model:None,thinking_level:None,
        service_tier:None,effective_service_tier:None,scoped_models:Vec::new(),goal_store_file:None,
        loaded_extension_paths:Vec::new(),signal:None,steering_signal:None,
        is_idle_fn:std::sync::Arc::new(||true),wait_for_idle_fn:std::sync::Arc::new(||Box::pin(async{})),is_project_trusted_fn:std::sync::Arc::new(||true),
        is_compacting_fn:std::sync::Arc::new(||false),get_system_prompt_fn:std::sync::Arc::new(||String::new()),
        get_system_prompt_options_fn:std::sync::Arc::new(||BuildSystemPromptOptions::default()),
        registered_mcp_servers:Vec::new(),update_tool_hook_status:None,idle_coordinator:None,logger:None,defer_macrotask:None, compaction_signal: Default::default() }
}
fn anthropic_model()->Model {
    serde_json::from_value(serde_json::json!({"id":"claude-sonnet-5-0","name":"Sonnet 5","api":"anthropic-messages","provider":"anthropic","baseUrl":"https://api.anthropic.com/v1","reasoning":true,"input":["text"],"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":200000,"maxTokens":8192})).expect("anthropic model fixture")
}
fn mcp_document()->maho_ext_tool_search::engine::document::ToolSearchDocument {
    maho_ext_tool_search::engine::document::ToolSearchDocument{name:"mcp_docs".into(),label:"MCP docs".into(),aliases:vec![],description:Some("documentation".into()),search_text:None,keywords:vec![],source:maho_ext_tool_search::engine::document::ToolSearchSource::Mcp,group:"docs".into(),owner_label:"server".into(),registration_id:"mcp:docs".into()}
}
#[derive(Default)]
struct TestSessionActions {active:std::sync::Mutex<Vec<String>>,catalog:std::sync::Arc<Catalog>}
impl maho_ext_api::ExtensionSessionActions for TestSessionActions {
    fn set_session_name(&self,_:&str)->Result<(),maho_ext_api::ExtensionFailure>{Err("unused".into())}
    fn get_session_name(&self)->Result<Option<String>,maho_ext_api::ExtensionFailure>{Err("unused".into())}
    fn set_label(&self,_:&str,_:Option<&str>)->Result<(),maho_ext_api::ExtensionFailure>{Err("unused".into())}
    fn execute_tool<'a>(&'a self,name:&'a str,_:JsonValue,_:maho_ext_api::ExecuteToolOptions)->maho_ext_api::ExecuteToolFuture<'a>{Box::pin(async move{Err(maho_ext_api::ExecuteToolError{code:maho_ext_api::ExecuteToolErrorCode::InactiveTool,tool_name:name.into(),message:"unused".into(),active_tools:Vec::new()})})}
    fn get_active_tools(&self)->Result<Vec<String>,maho_ext_api::ExtensionFailure>{Ok(self.active.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone())}
    fn set_active_tools(&self,names:Vec<String>)->Result<(),maho_ext_api::ExtensionFailure>{*self.active.lock().unwrap_or_else(std::sync::PoisonError::into_inner)=names;Ok(())}
    fn refresh_tools(&self)->Result<(),maho_ext_api::ExtensionFailure>{Ok(())}
    fn install_registered_tool(&self,tool:maho_ext_api::RegisteredTool)->Result<(),maho_ext_api::ExtensionFailure>{let maho_ext_api::RegisteredTool {definition,source_info}=tool;self.catalog.record(maho_ext_api::normalize_tool_exposure(&definition,source_info));Ok(())}
    fn register_removed_tool_hint(&self,_:&str,_:&str)->Result<(),maho_ext_api::ExtensionFailure>{Err("unused".into())}
    fn register_lazy_tool_activator(&self,_:maho_ext_api::LazyToolActivator)->Result<(),maho_ext_api::ExtensionFailure>{Ok(())}
    fn get_commands(&self)->Result<Vec<maho_ext_api::SlashCommandInfo>,maho_ext_api::ExtensionFailure>{Err("unused".into())}
    fn set_model(&self,_:Model)->ExtensionFuture<'_,bool>{Box::pin(async{Err("unused".into())})}
    fn get_thinking_level(&self)->Result<maho_ext_api::ThinkingLevel,maho_ext_api::ExtensionFailure>{Err("unused".into())}
    fn set_thinking_level(&self,_:maho_ext_api::ThinkingLevel)->Result<(),maho_ext_api::ExtensionFailure>{Err("unused".into())}
    fn set_session_model(&self,_:Model)->ExtensionFuture<'_,bool>{Box::pin(async{Err("unused".into())})}
    fn set_session_thinking_level(&self,_:maho_ext_api::ThinkingLevel)->Result<(),maho_ext_api::ExtensionFailure>{Err("unused".into())}
    fn set_session_fast_mode(&self,_:bool)->Result<(),maho_ext_api::ExtensionFailure>{Err("unused".into())}
    fn exec<'a>(&'a self,_:&'a str,_:&'a [String],_:&'a std::path::Path,_:maho_ext_api::ExecOptions)->ExtensionFuture<'a,maho_ext_api::ExecResult>{Box::pin(async{Err("unused".into())})}
}
fn mcp_service(catalog:&std::sync::Arc<Catalog>)->(ExtensionRuntime,std::sync::Arc<tokio::sync::Mutex<maho_ext_tool_search::service::ToolSearchService>>,maho_ext_api::ExtensionApi) {
    let runtime=ExtensionRuntime::default();
    runtime.bind_session_actions(std::sync::Arc::new(TestSessionActions {active:std::sync::Mutex::new(Vec::new()),catalog:catalog.clone()}));
    let service=std::sync::Arc::new(tokio::sync::Mutex::new(maho_ext_tool_search::service::ToolSearchService::new(runtime.clone(),catalog.clone())));
    let mut api=maho_ext_api::ExtensionApi::new(maho_ext_api::LoadedExtension::new("tool-search",Default::default(),maho_ext_api::SourceInfo {source:"builtin".into(),..Default::default()}),Default::default(),Default::default(),runtime.clone());
    maho_ext_tool_search::index::ToolSearchExtension {actions:catalog.clone(),mcp_native_enabled:std::sync::Arc::new(||true)}.register_with_service(&mut api,service.clone());
    (runtime,service,api)
}
#[tokio::test]
async fn one_service_instance_serves_the_registered_tool_and_the_native_adapter() {
    let catalog=std::sync::Arc::new(Catalog::default());
    let (runtime,service,api)=mcp_service(&catalog);
    // The MCP registrar installs the real registered definition into the runtime; the bound session
    // actions record it into the one shared registry the service and the adapter both read.
    let mut mcp_api=maho_ext_api::ExtensionApi::new(maho_ext_api::LoadedExtension::new("mcp",Default::default(),maho_ext_api::SourceInfo {source:"mcp".into(),..Default::default()}),Default::default(),Default::default(),runtime);
    mcp_api.register_tool(mcp_docs_definition());
    service.lock().await.feed(vec![mcp_document()],std::sync::Arc::new(|_|Ok(()))).expect("mcp publication into the shared service");
    let handler=api.registered.handlers[&maho_ext_api::EventKind::BeforeProviderRequest][0].clone();
    let mut event=ExtensionEvent::BeforeProviderRequest {payload:serde_json::json!({"tools":[]}),model:Some(anthropic_model()),headers:None};
    let maho_ext_api::EventResult::ProviderPayload(payload)=handler(&mut event,&context()).await.expect("adapter request") else {panic!("provider request must return a payload")};
    assert!(payload["tools"].as_array().unwrap().iter().any(|tool|tool["name"]=="mcp_docs"&&tool["defer_loading"]==serde_json::json!(true)),"adapter must read the same service: {payload}");
    let injected=payload["tools"].as_array().unwrap().iter().find(|tool|tool["name"]=="mcp_docs").expect("registry-fed MCP document must be injected");
    assert_eq!(injected["input_schema"],serde_json::json!({"type":"object","properties":{"library":{"type":"string"}},"required":["library"]}),"adapter must carry the registered schema: {payload}");
    let tool=maho_ext_tool_search::tool::create_tool_search_tool(service.clone());
    let result=(tool.execute)(maho_tools::definition::ToolCall {id:"call",params:serde_json::json!({"query":"documentation","source":"mcp"}),signal:Default::default(),on_update:None,context:None}).await.expect("tool search");
    assert_eq!(result.details.clone().unwrap()["matched"],serde_json::json!(["mcp_docs"]));
    let text=result.content.iter().map(|block|match block {maho_ext_api::ToolContent::Text {text,..}=>text.clone(),_=>String::new()}).collect::<Vec<_>>().join(" ");
    assert!(text.contains("library"),"search result must surface the registered schema: {text}");
    assert!(service.lock().await.take_native_injection_failure().is_none(),"a resolved MCP definition is not a shared-service failure");
}
/// A catalog document with no registered schema is skipped by the adapter instead of being
/// injected with a null schema, and the skip must not be recorded as a shared-service failure.
#[tokio::test]
async fn schema_less_mcp_document_is_skipped_without_blaming_the_shared_service() {
    let catalog=std::sync::Arc::new(Catalog::default());
    let (_runtime,service,api)=mcp_service(&catalog);
    service.lock().await.feed(vec![mcp_document()],std::sync::Arc::new(|_|Ok(()))).expect("mcp publication into the shared service");
    let handler=api.registered.handlers[&maho_ext_api::EventKind::BeforeProviderRequest][0].clone();
    let mut event=ExtensionEvent::BeforeProviderRequest {payload:serde_json::json!({"tools":[]}),model:Some(anthropic_model()),headers:None};
    let maho_ext_api::EventResult::ProviderPayload(payload)=handler(&mut event,&context()).await.expect("adapter request") else {panic!("provider request must return a payload")};
    assert!(!payload["tools"].as_array().unwrap().iter().any(|tool|tool["name"]=="mcp_docs"),"a document without a registered schema must not be injected: {payload}");
    assert!(service.lock().await.take_native_injection_failure().is_none(),"a missing schema is a skip, not a shared-service failure");
    let tool=maho_ext_tool_search::tool::create_tool_search_tool(service.clone());
    let result=(tool.execute)(maho_tools::definition::ToolCall {id:"call",params:serde_json::json!({"query":"documentation","source":"mcp"}),signal:Default::default(),on_update:None,context:None}).await.expect("tool search");
    assert_eq!(result.details.unwrap()["matched"],serde_json::json!(["mcp_docs"]));
}
