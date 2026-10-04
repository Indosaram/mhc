use maho_ext_api::*;
use std::{path::Path, sync::{Arc}};

struct TestSession;
impl ToolSessionManager for TestSession {
    fn session_id(&self) -> &str { "session" }
    fn session_file(&self) -> Option<&Path> { None }
}
impl SessionManager for TestSession {
    fn get_entries(&self) -> Vec<SessionEntry> { Vec::new() }
    fn get_branch(&self) -> Vec<SessionEntry> { Vec::new() }
    fn get_leaf_id(&self) -> Option<String> { None }
    fn get_session_name(&self) -> Option<String> { None }
}
struct TestRegistry;
struct AuthRegistry { model:Model, auth:ResolvedRequestAuth }
impl ModelRegistry for AuthRegistry {
    fn get_all(&self)->Vec<Model> {vec![self.model.clone()]}
    fn get_available(&self)->Vec<Model> {self.get_all()}
    fn find(&self,_:&str,_:&str)->Option<Model> {Some(self.model.clone())}
    fn has_configured_auth(&self,_:&Model)->bool {true}
    fn get_api_key_for_provider<'a>(&'a self,_:&'a str)->ExtensionFuture<'a,Option<String>> {Box::pin(async {panic!("remote must use model-scoped auth")})}
    fn get_api_key_and_headers<'a>(&'a self,model:&'a Model)->ExtensionFuture<'a,ResolvedRequestAuth> {
        Box::pin(async move {assert_eq!(model,&self.model);Ok(self.auth.clone())})
    }
}
struct AuthSession { actions:Arc<PolicyActions>, branch:Vec<SessionEntry> }
struct FallbackRegistry(Option<ResolvedRequestAuth>);
impl ModelRegistry for FallbackRegistry {
    fn get_all(&self)->Vec<Model> {Vec::new()}
    fn get_available(&self)->Vec<Model> {Vec::new()}
    fn find(&self,_:&str,_:&str)->Option<Model> {None}
    fn has_configured_auth(&self,_:&Model)->bool {false}
    fn get_api_key_for_provider<'a>(&'a self,_:&'a str)->ExtensionFuture<'a,Option<String>> {Box::pin(async {Err("local fallback reached".into())})}
    fn get_api_key_and_headers<'a>(&'a self,_:&'a Model)->ExtensionFuture<'a,ResolvedRequestAuth> {Box::pin(async {self.0.clone().ok_or_else(||ExtensionFailure::new("auth lookup failed"))})}
}
struct AuthCheckpointActions;
impl ExtensionSessionActions for AuthCheckpointActions {
    fn get_thinking_level(&self)->Result<ThinkingLevel,ExtensionFailure> {Ok(ThinkingLevel::Minimal)}
    fn get_active_tools(&self)->Result<Vec<String>,ExtensionFailure> {Ok(Vec::new())}
    fn set_session_name(&self,_:&str)->Result<(),ExtensionFailure> {Err("unused".into())}
    fn get_session_name(&self)->Result<Option<String>,ExtensionFailure> {Err("unused".into())}
    fn set_label(&self,_:&str,_:Option<&str>)->Result<(),ExtensionFailure> {Err("unused".into())}
    fn execute_tool<'a>(&'a self,_:&'a str,_:JsonValue,_:ExecuteToolOptions)->ExecuteToolFuture<'a> {Box::pin(async {panic!("unused")})}
    fn set_active_tools(&self,_:Vec<String>)->Result<(),ExtensionFailure> {Err("unused".into())}
    fn refresh_tools(&self)->Result<(),ExtensionFailure> {Err("unused".into())}
    fn register_removed_tool_hint(&self,_:&str,_:&str)->Result<(),ExtensionFailure> {Err("unused".into())}
    fn register_lazy_tool_activator(&self,_:LazyToolActivator)->Result<(),ExtensionFailure> {Err("unused".into())}
    fn get_commands(&self)->Result<Vec<SlashCommandInfo>,ExtensionFailure> {Err("unused".into())}
    fn set_model(&self,_:Model)->ExtensionFuture<'_,bool> {Box::pin(async {Err("unused".into())})}
    fn set_thinking_level(&self,_:ThinkingLevel)->Result<(),ExtensionFailure> {Err("unused".into())}
    fn set_session_model(&self,_:Model)->ExtensionFuture<'_,bool> {Box::pin(async {Err("unused".into())})}
    fn set_session_thinking_level(&self,_:ThinkingLevel)->Result<(),ExtensionFailure> {Err("unused".into())}
    fn set_session_fast_mode(&self,_:bool)->Result<(),ExtensionFailure> {Err("unused".into())}
    fn exec<'a>(&'a self,_:&'a str,_:&'a [String],_:&'a Path,_:ExecOptions)->ExtensionFuture<'a,ExecResult> {Box::pin(async {Err("unused".into())})}
}
impl ToolSessionManager for AuthSession {
    fn session_id(&self)->&str {"auth-capture"}
    fn session_file(&self)->Option<&Path> {None}
}
impl SessionManager for AuthSession {
    fn get_entries(&self)->Vec<SessionEntry> {self.branch.clone()}
    fn get_branch(&self)->Vec<SessionEntry> {self.branch.clone()}
    fn get_leaf_id(&self)->Option<String> {Some("keep".into())}
    fn get_session_name(&self)->Option<String> {None}
    fn extension_context_actions(&self)->Option<&dyn ExtensionContextActions> {Some(self.actions.as_ref())}
}
impl ModelRegistry for TestRegistry {
    fn get_all(&self) -> Vec<Model> { Vec::new() }
    fn get_available(&self) -> Vec<Model> { Vec::new() }
    fn find(&self, _: &str, _: &str) -> Option<Model> { None }
    fn has_configured_auth(&self, _: &Model) -> bool { false }
    fn get_api_key_for_provider<'a>(&'a self, _: &'a str) -> ExtensionFuture<'a, Option<String>> { Box::pin(async { Ok(Some("faux".into())) }) }
}
struct TestUi;
impl ExtensionUi for TestUi {
    fn select<'a>(&'a self, _: &'a str, _: &'a [String], _: ExtensionUiDialogOptions) -> UiFuture<'a, Option<String>> { Box::pin(async { None }) }
    fn confirm<'a>(&'a self, _: &'a str, _: &'a str, _: ExtensionUiDialogOptions) -> UiFuture<'a, bool> { Box::pin(async { false }) }
    fn input<'a>(&'a self, _: &'a str, _: Option<&'a str>, _: ExtensionUiDialogOptions) -> UiFuture<'a, Option<String>> { Box::pin(async { None }) }
    fn notify(&self, _: &str, _: NotificationType) {}
    fn set_status(&self, _: &str, _: Option<&str>) {}
    fn set_widget(&self, _: &str, _: Option<WidgetContent>, _: ExtensionWidgetOptions) {}
    fn set_header(&self, _: Option<ComponentFactory>) {}
    fn set_footer(&self, _: Option<ComponentFactory>) {}
    fn set_title(&self, _: &str) {}
    fn paste_to_editor(&self, _: &str) {}
    fn set_editor_text(&self, _: &str) {}
    fn get_editor_text(&self) -> String { String::new() }
    fn custom(&self, _: ComponentFactory, _: CustomUiOptions) -> ExtensionFuture<'_, JsonValue> { Box::pin(async { Err("UI not available".into()) }) }
    fn theme(&self) -> Theme { Theme::default() }
}
fn context() -> ExtensionContext {
    ExtensionContext { ui: Arc::new(TestUi), mode: ExtensionMode::Print, has_ui: false, cwd: "/tmp".into(), agent_dir: "/tmp/agent".into(),
        session_manager: Arc::new(TestSession), model_registry: Arc::new(TestRegistry), model: None, thinking_level: None,
        service_tier: None, effective_service_tier: None, scoped_models: Vec::new(), goal_store_file: None,
        loaded_extension_paths: Vec::new(), signal: None, steering_signal: None,
        is_idle_fn: Arc::new(|| true), wait_for_idle_fn: Arc::new(|| Box::pin(async {})), is_project_trusted_fn: Arc::new(|| true),
        is_compacting_fn: Arc::new(|| false), get_system_prompt_fn: Arc::new(|| "base".into()),
        get_system_prompt_options_fn: Arc::new(|| BuildSystemPromptOptions { cwd: "/tmp".into(), ..Default::default() }),
        registered_mcp_servers: Vec::new(), update_tool_hook_status: None, idle_coordinator: None, logger: None, defer_macrotask: None, compaction_signal: Default::default() }
}

struct PolicyActions { settings: std::sync::Mutex<ResolvedCompactionSettings>, usage:Option<ContextUsage>, idle_probe:Option<tokio::sync::mpsc::UnboundedSender<()>> }
impl PolicyActions {
    fn new(settings:ResolvedCompactionSettings)->Self {Self {settings:std::sync::Mutex::new(settings),usage:None,idle_probe:None}}
}
impl ExtensionSessionSettings for PolicyActions {
    fn get_retry_fallback_settings(&self) -> RetryFallbackSettings { RetryFallbackSettings { model_fallback:false, chains:Default::default(), revert_policy:FallbackRevertPolicy::Never } }
    fn set_fallback_chain<'a>(&'a self, _: &'a str, _: &'a [String]) -> ExtensionFuture<'a, ()> { Box::pin(async { Ok(()) }) }
    fn remove_fallback_chain<'a>(&'a self, _: &'a str) -> ExtensionFuture<'a, ()> { Box::pin(async { Ok(()) }) }
    fn set_model_fallback_enabled(&self, _: bool) -> ExtensionFuture<'_, ()> { Box::pin(async { Ok(()) }) }
    fn set_fallback_revert_policy(&self, _: FallbackRevertPolicy) -> ExtensionFuture<'_, ()> { Box::pin(async { Ok(()) }) }
    fn reload(&self) -> ExtensionFuture<'_, ()> { Box::pin(async { Ok(()) }) }
    fn get_fallback_status(&self) -> Option<RetryFallbackStatus> { None }
}
impl ExtensionContextActions for PolicyActions {
    fn get_model(&self) -> Option<Model> { None }
    fn get_service_tier(&self) -> Option<ServiceTier> { None }
    fn get_scoped_models(&self) -> Vec<ScopedModel> { Vec::new() }
    fn get_agent_dir(&self) -> std::path::PathBuf { "/tmp/agent".into() }
    fn is_idle(&self) -> bool { if let Some(probe)=&self.idle_probe {let _=probe.send(());} true }
    fn is_project_trusted(&self) -> bool { true }
    fn get_signal(&self) -> Option<AbortSignal> { None }
    fn abort(&self, _: Option<AbortSource>) {}
    fn has_pending_messages(&self) -> bool { false }
    fn request_reload(&self) -> ExtensionFuture<'_, ()> { Box::pin(async { Ok(()) }) }
    fn is_compacting(&self) -> bool { false }
    fn check_reload_veto(&self) -> ExtensionFuture<'_, ReloadVetoDecision> { Box::pin(async { Ok(ReloadVetoDecision {cancelled:false,reason:None}) }) }
    fn shutdown(&self) {}
    fn get_context_usage(&self) -> Option<ContextUsage> { self.usage.clone().or(Some(ContextUsage {tokens:Some(0),context_window:100_000,percent:Some(0.)})) }
    fn get_compaction_settings(&self) -> CompactionSettings { CompactionSettings {enabled:true,reserve_tokens:100,keep_recent_tokens:200} }
    fn get_resolved_compaction_settings(&self) -> Option<ResolvedCompactionSettings> { Some(self.settings.lock().expect("native compaction scenario invariant").clone()) }
    fn get_prompt_cache_safe_wait_seconds(&self) -> Option<f64> { None }
    fn get_prompt_cache_goal_backstop_max_seconds(&self) -> f64 { 0. }
    fn get_prompt_cache_keep_alive_settings(&self) -> PromptCacheKeepAliveSettings { PromptCacheKeepAliveSettings {enabled:false,max_requests_per_session:0,max_cost_usd_per_session:0.,margin_seconds:0.} }
    fn get_look_at_settings(&self) -> LookAtSettings { LookAtSettings {enabled:false,models:None} }
    fn get_ask_user_settings(&self) -> AskUserSettings { AskUserSettings {enabled:false,timeout_minutes:0.} }
    fn get_image_settings(&self) -> ImageSettings { ImageSettings {auto_resize:false,block_images:false} }
    fn session_settings(&self) -> &dyn ExtensionSessionSettings { self }
    fn compact(&self, _: CompactOptions) {}
    fn prepare_provider_request(&self, messages:Vec<AgentMessage>) -> ExtensionFuture<'_, ProviderRequestPreparation> {
        Box::pin(async move { Ok(ProviderRequestPreparation {messages,transform_payload:Arc::new(|payload|Box::pin(async move {Ok(payload)})),transform_headers:Arc::new(|headers|Box::pin(async move {Ok(headers)}))}) })
    }
    fn begin_compaction(&self, _: BeginCompactionOptions) -> Option<AbortSignal> { Some(AbortSignal::default()) }
    fn update_compaction(&self, _: UpdateCompactionOptions) {}
    fn end_compaction(&self, _: EndCompactionOptions) {}
    fn get_message_revision(&self) -> u64 { 0 }
    fn apply_compaction(&self, _: CompactionResult, _: ApplyCompactionOptions) -> ExtensionFuture<'_, ApplyCompactionResult> { Box::pin(async {Ok(ApplyCompactionResult::Rejected)}) }
    fn get_system_prompt(&self) -> String { "base".into() }
    fn get_system_prompt_options(&self) -> BuildSystemPromptOptions { Default::default() }
    fn get_loaded_hook_sources(&self) -> LoadedHookSources { LoadedHookSources {cwd:"/tmp".into(),agent_dir:"/tmp/agent".into(),global_hooks_path:"/tmp/global".into(),project_hooks_path:"/tmp/project".into(),global_settings_hooks:None,project_settings_hooks:None,global_hook_source_paths:Vec::new(),project_hook_source_paths:Vec::new(),pre_session_hook_source_paths:Vec::new(),runtime_hook_source_paths:Vec::new()} }
    fn kernel_tools(&self) -> Option<&dyn ExtensionKernelTools> { None }
}
struct PolicySession(Arc<PolicyActions>);
impl ToolSessionManager for PolicySession {
    fn session_id(&self) -> &str { "policy" }
    fn session_file(&self) -> Option<&Path> { None }
}
impl SessionManager for PolicySession {
    fn get_entries(&self) -> Vec<SessionEntry> { Vec::new() }
    fn get_branch(&self) -> Vec<SessionEntry> { Vec::new() }
    fn get_leaf_id(&self) -> Option<String> { None }
    fn get_session_name(&self) -> Option<String> { None }
    fn extension_context_actions(&self) -> Option<&dyn ExtensionContextActions> { Some(self.0.as_ref()) }
}

#[tokio::test]
async fn registered_context_consumes_live_tool_admission_gate() {
    run_policy_scenario().await;
}

async fn run_policy_scenario() {
    let settings = policy_settings();
    let actions=Arc::new(PolicyActions::new(settings));
    let mut ctx=context();ctx.session_manager=Arc::new(PolicySession(Arc::clone(&actions)));
    let mut api=ExtensionApi::new(LoadedExtension::new("compaction","/tmp".into(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),ExtensionRuntime::default());
    maho_ext_compaction::CompactionExtension.register(&mut api);
    let source=vec![serde_json::from_value(serde_json::json!({"role":"assistant","content":[{"type":"toolCall","id":"call","name":"read","arguments":{}}],"api":"faux","provider":"faux","model":"m","usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}},"stopReason":"toolUse","timestamp":0})).expect("native compaction scenario invariant"),serde_json::from_value(serde_json::json!({"role":"toolResult","toolCallId":"call","toolName":"read","content":[{"type":"text","text":"x".repeat(30000)}],"isError":false,"timestamp":0})).expect("native compaction scenario invariant")];
    let mut event=ExtensionEvent::Context {messages:source.clone()};
    let result=api.registered.handlers[&EventKind::Context][0](&mut event,&ctx).await.expect("native compaction scenario invariant");
    let EventResult::Context {messages:Some(preserved)}=result else {panic!("missing registered context")};
    assert_eq!(serde_json::to_value(&preserved).expect("native compaction scenario invariant"),serde_json::to_value(&source).expect("native compaction scenario invariant"));
    actions.settings.lock().expect("native compaction scenario invariant").tool_admission_enabled=true;
    let mut event=ExtensionEvent::Context {messages:source};
    let result=api.registered.handlers[&EventKind::Context][0](&mut event,&ctx).await.expect("native compaction scenario invariant");
    let EventResult::Context {messages:Some(projected)}=result else {panic!("missing registered context")};
    assert!(serde_json::to_value(projected).expect("native compaction scenario invariant")[1]["content"][0]["text"].as_str().expect("native compaction scenario invariant").len()<30000);
}

fn policy_settings() -> ResolvedCompactionSettings {
    ResolvedCompactionSettings {enabled:true,reserve_tokens:100,keep_recent_tokens:200,speculative_enabled:false,speculative_fraction:0.42,speculative_cooldown_ms:0.,restoration_enabled:false,restoration_max_items:1.,restoration_max_tokens_per_item:100.,restoration_max_total_tokens:100.,restoration_context_ratio:0.01,idle_compaction_enabled:false,grace_band_enabled:false,tool_admission_enabled:false,reminder_enabled:false,reserve_scaling_enabled:false,speculative_lead_tokens:None,summarization_max_duration_ms:None}
}

#[tokio::test]
async fn registered_restoration_is_bounded_accepted_only_and_once() {
    for enabled in [false,true] {
        let mut settings=policy_settings();settings.restoration_enabled=enabled;
        let actions=Arc::new(PolicyActions::new(settings));
        let mut ctx=context();ctx.session_manager=Arc::new(PolicySession(actions));
        let mut api=ExtensionApi::new(LoadedExtension::new("compaction","/tmp".into(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),ExtensionRuntime::default());
        maho_ext_compaction::CompactionExtension.register(&mut api);
        for path in ["one.rs","two.rs"] {
            let mut event=ExtensionEvent::ToolCall(ToolCallEvent {tool_call_id:path.into(),tool_name:"read".into(),input:serde_json::json!({"path":path})});
            for handler in &api.registered.handlers[&EventKind::ToolCall] {handler(&mut event,&ctx).await.expect("native compaction scenario invariant");}
        }
        let mut rejected=ExtensionEvent::SessionCompact(SessionCompactEvent::Rejected {reason:CompactionReason::Manual,request_id:"rejected".into(),rejection_cause:CompactionRejectionCause::ExternalOwner});
        for handler in &api.registered.handlers[&EventKind::SessionCompact] {handler(&mut rejected,&ctx).await.expect("native compaction scenario invariant");}
        let mut before=ExtensionEvent::BeforeAgentStart(BeforeAgentStartEvent {prompt:"continue".into(),images:None,system_prompt:"base".into(),system_prompt_options:Default::default()});
        let handler=&api.registered.handlers[&EventKind::BeforeAgentStart][0];
        let result=handler(&mut before,&ctx).await.expect("native compaction scenario invariant");
        assert!(matches!(result,EventResult::BeforeAgentStart(BeforeAgentStartEventResult {message:None,..})));
        let mut accepted=ExtensionEvent::SessionCompact(SessionCompactEvent::Accepted {reason:CompactionReason::Manual,request_id:"accepted".into(),compaction_entry:SessionEntry {id:"compact".into(),parent_id:None,timestamp:String::new(),kind:"compaction".into(),data:serde_json::json!({"firstKeptEntryId":"keep"})},from_extension:true,will_retry:false});
        for handler in &api.registered.handlers[&EventKind::SessionCompact] {handler(&mut accepted,&ctx).await.expect("native compaction scenario invariant");}
        let result=handler(&mut before,&ctx).await.expect("native compaction scenario invariant");
        let EventResult::BeforeAgentStart(output)=result else {panic!("missing start result")};
        assert_eq!(output.message.is_some(),enabled);
        if let Some(message)=output.message {assert_eq!(message.details.expect("native compaction scenario invariant")["items"].as_array().expect("native compaction scenario invariant").len(),1);}
        let result=handler(&mut before,&ctx).await.expect("native compaction scenario invariant");
        assert!(matches!(result,EventResult::BeforeAgentStart(BeforeAgentStartEventResult {message:None,..})));
    }
}

#[tokio::test]
async fn provider_owned_model_selection_stands_down_before_unbound_actions() {
    let mut api = ExtensionApi::new(LoadedExtension::new("compaction","/tmp".into(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),ExtensionRuntime::default());
    maho_ext_compaction::CompactionExtension.register(&mut api);
    let mut ctx = context();
    let model: Model = serde_json::from_value(serde_json::json!({"id":"m","name":"m","api":"anthropic-messages","provider":"anthropic-subscription","baseUrl":"","reasoning":false,"input":["text"],"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":100000,"maxTokens":1000})).expect("native compaction scenario invariant");
    ctx.model=Some(model.clone());
    let mut event=ExtensionEvent::ModelSelect(ModelSelectEvent {model,previous_model:None,source:ModelSelectSource::Set,system_prompt:String::new(),system_prompt_options:Default::default()});
    for handler in &api.registered.handlers[&EventKind::ModelSelect] {assert!(matches!(handler(&mut event,&ctx).await.expect("native compaction scenario invariant"),EventResult::None));}
}

#[tokio::test]
async fn native_session_compaction_uses_registered_generator_and_persists_metadata() {
    run_native_scenario(false, false).await;
}

async fn run_native_scenario(cancel: bool, threshold: bool) {
    run_native_variant(cancel,threshold,"summary").await;
}

async fn run_native_variant(cancel: bool, threshold: bool, variant: &str) {
    use maho_core::agent_session::{AgentSession, AgentSessionConfig};
    use maho_ai::providers::faux::{RegisterFauxProviderOptions, FauxAssistantMessageOptions, faux_assistant_message, register_faux_provider, faux_provider};
    let temp = tempfile::tempdir().expect("native compaction scenario invariant");
    let cwd = temp.path().to_string_lossy().into_owned();
    let idle_variant=matches!(variant,"idle-shutdown"|"idle-model-change"|"idle-stale-runner");
    let provider = register_faux_provider(RegisterFauxProviderOptions { api: Some("compaction-registration-faux".into()), tokens_per_second: Some(0.), ..Default::default() });
    let mut model = provider.get_model(None).expect("native compaction scenario invariant");
    let (remote_started,mut remote_started_rx)=tokio::sync::mpsc::channel(1);
    let remote_cancel=matches!(variant,"remote-cancel"|"session-abort");
    let remote_failure=matches!(variant,"remote-auth"|"remote-network");
    let remote_auth=variant=="remote-auth";
    let remote_network=variant=="remote-network";
    let remote_server = if matches!(variant,"remote-http"|"remote-sse"|"remote-sse-success"|"remote-cancel"|"session-abort"|"remote-auth"|"remote-network") {
        use tokio::io::{AsyncReadExt,AsyncWriteExt};
        let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("local remote listener");
        model.api="openai-responses".into();model.provider="openai".into();model.base_url=format!("http://{}/v1",listener.local_addr().expect("listener address"));
        let sse=matches!(variant,"remote-sse"|"remote-sse-success");
        let opaque_success=variant=="remote-sse-success";
        if sse || remote_failure {
            maho_ai::api_registry::register_builtin_api_provider("openai-responses",Arc::new(NativeResponsesStreams));
        }
        if sse {
            model.compat=Some(maho_ai::types::ModelCompat(serde_json::Map::from_iter([("supportsRemoteCompactionV2".into(),serde_json::json!(true)),("supportsWebSocket".into(),serde_json::json!(false))])));
        }
        Some(tokio::spawn(async move {
            for attempt in 0..if (sse && !opaque_success) || remote_failure {2} else {1} {
            let (mut socket,_)=listener.accept().await.expect("registered remote connection");
            let mut bytes=Vec::new();let mut buffer=[0;4096];
            let body=loop {
                let count=socket.read(&mut buffer).await.expect("read remote request");assert!(count>0);bytes.extend_from_slice(&buffer[..count]);
                if let Some(boundary)=bytes.windows(4).position(|window|window==b"\r\n\r\n") {
                    let headers=String::from_utf8_lossy(&bytes[..boundary]);
                    let length:usize=headers.lines().find_map(|line|line.to_ascii_lowercase().strip_prefix("content-length: ").map(str::to_owned)).expect("request length").parse().expect("numeric length");
                    if bytes.len()>=boundary+4+length {assert!(headers.starts_with(if (sse && attempt==0) || (remote_failure && attempt==1) {"POST /v1/responses HTTP/1.1"} else {"POST /v1/responses/compact HTTP/1.1"}),"captured request line: {:?}",headers.lines().next());assert!(headers.contains("authorization: Bearer faux"));break serde_json::from_slice::<serde_json::Value>(&bytes[boundary+4..boundary+4+length]).expect("captured remote JSON");}
                }
            };
            assert!(body["input"].as_array().is_some_and(|input|!input.is_empty()));
            if !remote_failure || attempt==0 {assert!(body["prompt_cache_key"].is_string());}
            if remote_network && attempt==0 {continue;}
            if remote_auth && attempt==0 {
                socket.write_all(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.expect("auth failure response");
                continue;
            }
            if remote_failure && attempt==1 {
                assert_eq!(body["stream"],true);
                let response="data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"id\":\"summary-message\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[]}}\n\ndata: {\"type\":\"response.output_text.delta\",\"output_index\":0,\"delta\":\"<summary>native checkpoint</summary>\"}\n\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"local-summary\",\"status\":\"completed\",\"output\":[{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"<summary>native checkpoint</summary>\"}]}]}}\n\n";
                socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",response.len()).as_bytes()).await.expect("local summary SSE response");
                continue;
            }
            if remote_cancel {
                remote_started.send(()).await.expect("published remote request event");
                assert_eq!(socket.read(&mut buffer).await.expect("remote cancellation disconnect"),0);
                break;
            }
            if sse && attempt==0 {
                assert_eq!(body["stream"],true);
                assert!(body["input"].as_array().expect("SSE input").iter().any(|item|item["type"]=="compaction_trigger"));
                let response=if opaque_success {"data: {\"type\":\"response.output_item.added\",\"output_index\":0,\"item\":{\"id\":\"native-checkpoint\",\"type\":\"compaction\",\"encrypted_content\":\"opaque-sse\"}}\n\ndata: {\"type\":\"response.completed\",\"response\":{\"id\":\"sse\",\"status\":\"completed\",\"output\":[{\"id\":\"native-checkpoint\",\"type\":\"compaction\",\"encrypted_content\":\"opaque-sse\"}]}}\n\n"} else {"data: {\"type\":\"response.completed\",\"response\":{\"id\":\"sse\",\"status\":\"completed\",\"output\":[]}}\n\n"};
                socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",response.len()).as_bytes()).await.expect("native SSE response");
                continue;
            }
            let response=serde_json::json!({"id":"remote","object":"response.compaction","created_at":0,"output":[{"type":"compaction","encrypted_content":"opaque"}]}).to_string();
            socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",response.len()).as_bytes()).await.expect("remote response");
            }
        }))
    } else {None};
    if threshold || variant=="reminder" {model.context_window=40000;}
    if idle_variant {model.context_window=48000;}
    let (started, mut started_rx) = tokio::sync::mpsc::channel(1);
    let idle_started=started.clone();
    provider.set_responses(if cancel { vec![maho_ai::providers::faux::FauxResponseStep::Factory(Arc::new(move |_,options,_,_| {
        let started=started.clone();
        let signal=options.and_then(|options|options.stream.request.signal.clone()).expect("native compaction scenario invariant");
        Box::pin(async move {started.send(()).await.expect("native compaction scenario invariant");signal.cancelled().await;faux_assistant_message(Vec::<ContentBlock>::new(),FauxAssistantMessageOptions {stop_reason:Some(maho_ai::types::StopReason::Aborted),..Default::default()})})
    }))] } else {vec![faux_assistant_message(vec![ContentBlock::text("<summary>native checkpoint</summary>")], FauxAssistantMessageOptions { timestamp: Some(0), ..Default::default() }).into()]});
    if threshold || variant=="reminder" {provider.append_responses(vec![faux_assistant_message(vec![ContentBlock::text("continued")],Default::default()).into()]);}
    let (idle_cancelled,mut idle_cancelled_rx)=tokio::sync::mpsc::channel(1);
    if idle_variant {
        let response=faux_assistant_message(vec![ContentBlock::text("ready")],Default::default());
        provider.set_responses(vec![response.into(),maho_ai::providers::faux::FauxResponseStep::Factory(Arc::new(move |_,options,_,_| {
            let started=idle_started.clone();let cancelled=idle_cancelled.clone();
            let signal=options.and_then(|options|options.stream.request.signal.clone()).expect("idle summary signal");
            Box::pin(async move {
                started.send(()).await.expect("idle request subscription");signal.cancelled().await;
                cancelled.send(()).await.expect("idle cancellation subscription");
                faux_assistant_message(Vec::<ContentBlock>::new(),FauxAssistantMessageOptions {stop_reason:Some(maho_ai::types::StopReason::Aborted),..Default::default()})
            })
        }))]);
    }
    if variant=="overflow" {
        provider.set_responses(vec![maho_ai::utils::lazy::setup_error_message(&model,"maximum context length exceeded").into(),faux_assistant_message(vec![ContentBlock::text("summary after shrink")],Default::default()).into(),faux_assistant_message(vec![ContentBlock::text("continued")],Default::default()).into()]);
    } else if variant=="fallback" {
        provider.set_responses(vec![faux_assistant_message(Vec::<ContentBlock>::new(),Default::default()).into(),faux_assistant_message(vec![ContentBlock::text("continued")],Default::default()).into()]);
    }
    let mut credentials = maho_core::auth_storage::AuthStorage::in_memory(Default::default());
    credentials.set(&model.provider,Some(serde_json::json!({"type":"api_key","key":"faux"}))).expect("native compaction scenario invariant");
    let native = faux_provider(RegisterFauxProviderOptions { api: Some(model.api.clone()), provider: Some(model.provider.clone()), tokens_per_second: Some(0.), ..Default::default() });
    let runtime = maho_core::model_runtime::ModelRuntime::create_sync(maho_core::model_runtime::CreateModelRuntimeOptions {
        models_path: Some(temp.path().join("models.json")), credentials: Some(Arc::new(credentials)), providers: Some(vec![native.provider]), ..Default::default()
    });
    let mut manager = maho_core::session_manager::SessionManager::in_memory(&cwd,None,None);
    manager.append_message(serde_json::json!({"role":"user","content":"old ".repeat(if variant=="overflow" {2000} else if idle_variant {25000} else {30000}),"timestamp":0}));
    manager.append_message(serde_json::json!({"role":"assistant","content":[{"type":"text","text":"reply"}],"api":model.api,"provider":model.provider,"model":model.id,"usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}},"stopReason":"stop","timestamp":0}));
    if variant=="overflow" {
        manager.append_message(serde_json::json!({"role":"user","content":"middle ".repeat(1000),"timestamp":0}));
        manager.append_message(serde_json::json!({"role":"assistant","content":[{"type":"text","text":"middle reply"}],"api":model.api,"provider":model.provider,"model":model.id,"usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}},"stopReason":"stop","timestamp":0}));
    }
    manager.append_message(serde_json::json!({"role":"user","content":"continue","timestamp":0}));
    if threshold || variant=="reminder" {
        let input=if variant=="reminder" {20000} else {30000};
        manager.append_message(serde_json::json!({"role":"assistant","content":[{"type":"text","text":"ready"}],"api":model.api,"provider":model.provider,"model":model.id,"usage":{"input":input,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":input,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}},"stopReason":"stop","timestamp":0}));
    }
    let agent=maho_agent_for_test(model);
    if threshold || variant=="reminder" || idle_variant {agent.set_messages(manager.build_context(manager.leaf_id()).messages.into_iter().map(|value|serde_json::from_value(value).expect("seeded native message")).collect());}
    let storage = maho_core::settings_manager::InMemorySettingsStorage::default();
    maho_core::settings_manager::SettingsStorage::with_lock(&storage, maho_core::settings_manager::SettingsScope::Global,
        &mut |_|Some(if variant=="fractional" {serde_json::json!({"compaction":{"keepRecentTokens":1,"reserveTokens":100,"speculativeEnabled":false,"speculativeFraction":0.42,"speculativeCooldownMs":321.5,"restorationEnabled":false,"restorationMaxItems":2.5,"restorationMaxTokensPerItem":11.5,"restorationMaxTotalTokens":22.5,"restorationContextRatio":0.03,"idleCompactionEnabled":false,"graceBandEnabled":false,"toolAdmissionEnabled":false,"reminderEnabled":false,"reserveScalingEnabled":false,"speculativeLeadTokens":12000.5,"summarizationMaxDurationMs":90000.5}})} else {serde_json::json!({"compaction":{"keepRecentTokens":1}})}.to_string())).expect("native compaction scenario invariant");
    let session = AgentSession::new(AgentSessionConfig {
        agent, session_manager: manager,
        settings_manager: maho_core::settings_manager::SettingsManager::from_storage(Box::new(storage),false),
        cwd: cwd.clone(), agent_dir: Some(cwd), fallback_now: Some(Arc::new(||0.)), retry_random: Some(Arc::new(||0.5)),
        scoped_models: Vec::new(), favorite_models: Vec::new(), flag_values: Default::default(), custom_tools: Vec::new(), model_runtime: Some(runtime), model_registry: None,
        uses_default_stream_function: Some(false), initial_active_tool_names: None, default_tool_names: None, eval_only_tool_names: None, allowed_tool_names: None, excluded_tool_names: None, base_tools_override: None, session_start_event: None, auto_title_sessions: Some(false),
    }).expect("native compaction scenario invariant");
    let observed_signal=Arc::new(std::sync::Mutex::new(None));
    if variant=="reminder" {
        session.with_settings_manager_mut(|manager|manager.set(maho_core::settings_manager::SettingsScope::Global,&serde_json::Map::from_iter([("compaction".into(),serde_json::json!({"keepRecentTokens":1,"reserveTokens":100,"speculativeEnabled":false,"idleCompactionEnabled":false,"reminderEnabled":true}))]))).expect("reminder policy");
    }
    let mut runner_context=context();if idle_variant {runner_context.mode=ExtensionMode::Rpc;}
    let retired_context=Arc::new(std::sync::Mutex::new(None));
    session.set_extension_runner(maho_ext_host::ExtensionRunner::from_static(vec![Box::new(CancellationObserver(Arc::clone(&observed_signal))),Box::new(IdleContextObserver(Arc::clone(&retired_context))),Box::new(maho_ext_compaction::CompactionExtension)],runner_context)).await;
    if idle_variant {
        session.bind_extensions(maho_core::agent_session::ExtensionBindings {mode:Some(ExtensionMode::Rpc),..Default::default()}).await;
        let prompted=tokio::time::timeout(std::time::Duration::from_secs(10),session.prompt("first",Default::default())).await;
        let outcome=tokio::time::timeout(std::time::Duration::from_secs(10),async {
            started_rx.recv().await.ok_or_else(||"idle request closed".to_string())?;
            Ok::<_,String>(())
        }).await;
        let before=session.with_session_manager(|manager|manager.entries());
        let superseded=if variant=="idle-model-change" {
            let mut selected=session.model();selected.id="replacement-model".into();selected.context_window=100000;
            Some(tokio::time::timeout(std::time::Duration::from_secs(10),session.set_session_model(selected)).await)
        } else {None};
        let model_cancelled=if superseded.is_some() {Some(tokio::time::timeout(std::time::Duration::from_secs(10),idle_cancelled_rx.recv()).await)} else {None};
        let shutdown=tokio::time::timeout(std::time::Duration::from_secs(10),session.emit_session_shutdown(SessionReason::Quit)).await;
        let disposed=tokio::time::timeout(std::time::Duration::from_secs(10),session.dispose()).await;
        let cancelled=match model_cancelled {Some(cancelled)=>cancelled,None=>tokio::time::timeout(std::time::Duration::from_secs(10),idle_cancelled_rx.recv()).await};
        let calls=provider.get_call_log().len();provider.unregister();
        assert!(matches!(prompted,Ok(Ok(_))),"foreground outcome: {prompted:?}");
        assert!(matches!(outcome,Ok(Ok(()))),"idle request outcome: {outcome:?}");
        assert!(shutdown.is_ok(),"bounded registered shutdown dispatch");
        if let Some(superseded)=superseded {assert!(matches!(superseded,Ok(Ok(_))),"bounded actual model change: {superseded:?}");}
        assert!(disposed.is_ok(),"bounded idle shutdown cleanup");
        assert!(matches!(cancelled,Ok(Some(()))),"shutdown cancels actual idle provider signal");
        if variant=="idle-stale-runner" {
            let retired=retired_context.lock().expect("captured idle context").clone().expect("actual agent-end context");
            assert!(retired.get_message_revision().is_err(),"retired runner rejects reads");
            let applied=tokio::time::timeout(std::time::Duration::from_secs(10),retired.apply_compaction(CompactionResult {summary:"late summary".into(),first_kept_entry_id:String::new(),tokens_before:25017,details:None},ApplyCompactionOptions {reason:CompactionReason::Extension,expected_revision:None,expected_warm_anchor:None,signal:None})).await;
            assert!(matches!(applied,Ok(Err(_))),"retired runner rejects late compaction: {applied:?}");
        }
        assert_eq!(calls,2,"one foreground request and one idle summary, no replacement");
        assert!(!before.iter().any(|entry|entry["type"]=="compaction"));
        let after=session.with_session_manager(|manager|manager.entries());
        if variant=="idle-model-change" {assert!(!after.iter().any(|entry|entry["type"]=="compaction"),"retired model job cannot apply");}
        else {assert_eq!(after,before,"no late idle apply after shutdown");}
        println!("PASS {variant}: real RPC-mode agent-end starts idle summary; registered supersession cancels it without late apply; cleanup complete");return;
    }
    if variant=="fractional" {
        session.set_extension_runner(maho_ext_host::ExtensionRunner::from_static(vec![Box::new(LiveFractionalObserver),Box::new(maho_ext_compaction::CompactionExtension)],context())).await;
    }
    if variant=="lifecycle" {
        let captured=Arc::new(std::sync::Mutex::new(Vec::new()));
        let events=Arc::clone(&captured);
        let _subscription=session.subscribe(Arc::new(move |event| {
            let reason_name=|reason:&maho_ext_api::CompactionReason|match reason {
                maho_ext_api::CompactionReason::Manual=>"manual",maho_ext_api::CompactionReason::Threshold=>"threshold",
                maho_ext_api::CompactionReason::Overflow=>"overflow",maho_ext_api::CompactionReason::PrePrompt=>"pre-prompt",
                maho_ext_api::CompactionReason::Branch=>"branch",maho_ext_api::CompactionReason::Extension=>"extension",
            };
            let value=match event {
                maho_ext_api::AgentSessionEvent::CompactionStart {reason,..}=>Some(serde_json::json!({"type":"compaction_start","reason":reason_name(reason)})),
                maho_ext_api::AgentSessionEvent::CompactionEnd {reason,accepted,aborted,..}=>Some(serde_json::json!({"type":"compaction_end","reason":reason_name(reason),"accepted":accepted,"aborted":aborted})),
                _=>None,
            };
            if let Some(value)=value {events.lock().expect("lifecycle events").push(value);}
        }));
        let outcome=tokio::time::timeout(std::time::Duration::from_secs(10),session.compact(None)).await;
        let entries=session.with_session_manager(|manager|manager.entries());
        let requests:Vec<_>=provider.get_call_log().iter().map(|call|serde_json::json!({"roles":call.context.messages.iter().map(|message|serde_json::to_value(message).expect("request message")["role"].clone()).collect::<Vec<_>>()})).collect();
        let events=captured.lock().expect("lifecycle events").clone();
        let disposed=tokio::time::timeout(std::time::Duration::from_secs(10),session.dispose()).await;
        provider.unregister();
        assert!(disposed.is_ok(),"bounded lifecycle cleanup");
        let result=outcome.expect("bounded lifecycle compaction").expect("lifecycle result");
        let actual=serde_json::json!({"summary":result.summary,"tokensBefore":result.tokens_before,"lifecycle":events,"compactions":entries.iter().filter(|entry|entry["type"]=="compaction").count(),"requests":requests});
        let reference:serde_json::Value=serde_json::from_str(include_str!("golden-lifecycle.json")).expect("source lifecycle golden");
        assert_eq!(actual,reference);
        println!("PASS lifecycle: pinned source events, request roles and persisted compaction agree; cleanup complete");
        return;
    }
    if variant=="reminder" {
        for prompt in ["first","second"] {tokio::time::timeout(std::time::Duration::from_secs(10),session.prompt(prompt,Default::default())).await.expect("bounded reminder prompt").expect("reminder prompt");}
        let calls=provider.get_call_log();assert_eq!(calls.len(),2);
        assert_ne!(calls[0].context.system_prompt,calls[1].context.system_prompt,"reminder lease must not repeat on the next request");
        assert!(!session.with_session_manager(|manager|manager.entries()).iter().any(|entry|entry["type"]=="compaction"));
        session.dispose().await;provider.unregister();println!("PASS reminder: real repeated provider requests consume one epoch lease; cleanup: disposed session, unregistered faux, tempdir dropped");return;
    }
    if cancel || remote_cancel {
        let operation=session.compact(None);tokio::pin!(operation);
        let outcome=tokio::time::timeout(std::time::Duration::from_secs(10),async {
            let request_started=async {if remote_cancel {remote_started_rx.recv().await} else {started_rx.recv().await}};
            tokio::select! { result=&mut operation=>return Err(format!("completed before request subscription: {result:?}")), started=request_started=>if started!=Some(()) {return Err("request event closed".into());} }
            if variant=="session-abort" {session.abort().await;} else {observed_signal.lock().expect("native compaction scenario invariant").as_ref().expect("native compaction scenario invariant").abort();}
            Ok(operation.await.is_err())
        }).await;
        let persisted=session.with_session_manager(|manager|manager.entries());
        let calls=provider.get_call_log().len();
        let compacting=session.is_compacting();
        let server_result=if let Some(mut server)=remote_server {
            let result=tokio::time::timeout(std::time::Duration::from_secs(10),&mut server).await;
            if result.is_err() {server.abort();let _=server.await;}
            Some(result)
        } else {None};
        let disposed=tokio::time::timeout(std::time::Duration::from_secs(10),session.dispose()).await;provider.unregister();
        assert!(disposed.is_ok(),"bounded negative cleanup");
        assert!(matches!(outcome,Ok(Ok(true))),"cancellation outcome: {outcome:?}");
        assert!(!compacting,"settled cancellation must clear active request");
        assert!(!persisted.iter().any(|entry|entry["type"]=="compaction"));
        assert_eq!(session.with_session_manager(|manager|manager.entries()),persisted,"no late apply after disposal");
        assert_eq!(calls,if remote_cancel {0} else {1});
        if let Some(result)=server_result {assert!(matches!(result,Ok(Ok(()))),"remote disconnect cleanup: {result:?}");}
        println!("PASS cancellation: one subscribed request, no persisted compaction; cleanup: disposed session, unregistered faux, tempdir dropped");
        return;
    }
    if threshold {
        assert!(session.get_context_usage().and_then(|usage|usage.tokens).is_some_and(|tokens|tokens>25000),"seeded usage must exceed prompt budget");
        tokio::time::timeout(std::time::Duration::from_secs(10),session.prompt("continue now",Default::default())).await.expect("native compaction scenario invariant").expect("native compaction scenario invariant");
        let compactions:Vec<_>=session.with_session_manager(|manager|manager.entries()).into_iter().filter(|entry|entry["type"]=="compaction").collect();
        let origin=if variant=="fallback" {"required-compaction-recovery"} else {"core-route"};
        assert!(compactions.iter().any(|entry|entry["details"]["origin"]==origin),"compactions: {compactions:?}");
    } else {
        let result = tokio::time::timeout(std::time::Duration::from_secs(10),session.compact(None)).await.expect("native compaction scenario invariant").expect("native compaction scenario invariant");
        if variant=="remote-sse-success" {
            let details=result.details.as_ref().expect("opaque SSE details");
            assert_eq!(details["transport"],"responses-v2");assert_eq!(details["replacementInput"][0]["encrypted_content"],"opaque-sse");
        } else if matches!(variant,"remote-http"|"remote-sse") {
            assert_eq!(result.details.as_ref().expect("remote details")["transport"],"compact-endpoint");
        } else {assert_eq!(result.details.as_ref().expect("native compaction scenario invariant")["origin"],if variant=="fallback" {"required-compaction-recovery"} else {"core-route"},"details={:?}, calls={:?}",result.details,provider.get_call_log());}
        if remote_failure {assert_eq!(provider.get_call_log().len(),0,"fallback uses actual native Responses transport");}
        if variant=="fallback" {
            tokio::time::timeout(std::time::Duration::from_secs(10),session.prompt("continue after recovery",Default::default())).await.expect("bounded postfallback continuation").expect("postfallback prompt succeeds");
            assert!(session.with_session_manager(|manager|manager.entries()).iter().any(|entry|entry["message"]["role"]=="assistant" && entry["message"]["content"].as_array().is_some_and(|blocks|blocks.iter().any(|block|block["text"]=="continued"))));
        }
    }
    let entries = session.with_session_manager(|manager|manager.entries());
    assert!(entries.iter().any(|entry|entry["customType"] == "compaction.agent-checkpoint"));
    assert!(entries.iter().any(|entry|entry["customType"] == "compaction.todo-snapshot"));
    assert_eq!(provider.get_call_log().len(),if matches!(variant,"remote-http"|"remote-sse"|"remote-sse-success") || remote_failure {0} else if variant=="overflow" {3} else if threshold || variant=="fallback" {2} else {1});
    if variant=="remote-sse-success" {assert!(entries.iter().any(|entry|entry["type"]=="compaction" && entry["details"]["replacementInput"][0]["encrypted_content"]=="opaque-sse"),"native checkpoint persisted losslessly");}
    if variant=="overflow" {
        let calls=provider.get_call_log();
        let tokens=|index:usize|calls[index].context.messages.iter().map(|message|maho_core::compaction::compaction::estimate_tokens(&serde_json::to_value(message).expect("request message"))).sum::<u64>();
        assert!(tokens(1)<tokens(0),"overflow retry must shrink actual billed summary input: first={} retry={}",tokens(0),tokens(1));
    }
    if let Some(server)=remote_server {tokio::time::timeout(std::time::Duration::from_secs(10),server).await.expect("remote server cleanup bound").expect("remote capture assertions");}
    session.dispose().await;
    provider.unregister();
    println!("PASS {variant}: registered generation accepted, checkpoint and todos persisted; cleanup: disposed session, unregistered faux, tempdir dropped");
}

#[tokio::test]
async fn native_registered_cancellation_is_subscribed_and_never_persists() {run_native_scenario(true,false).await;}

#[tokio::test]
async fn native_overthreshold_prompt_compacts_before_provider_turn() {run_native_scenario(false,true).await;}

#[tokio::test]
async fn native_overflow_shrinks_summary_request_before_continuation() {run_native_variant(false,true,"overflow").await;}

#[tokio::test]
async fn native_empty_summary_uses_required_deterministic_fallback() {run_native_variant(false,false,"fallback").await;}

#[tokio::test]
async fn native_registered_remote_compaction_posts_real_http() {run_native_variant(false,false,"remote-http").await;}

#[tokio::test]
async fn native_remote_auth_failure_falls_back_once() {run_native_variant(false,false,"remote-auth").await;}

#[tokio::test]
async fn native_remote_network_failure_falls_back_once() {run_native_variant(false,false,"remote-network").await;}

#[tokio::test]
async fn native_registered_remote_sse_falls_back_to_compact_endpoint() {run_native_variant(false,false,"remote-sse").await;}

#[tokio::test]
async fn native_registered_remote_abort_disconnects_without_local_summary() {run_native_variant(false,false,"remote-cancel").await;}

#[tokio::test]
async fn native_session_abort_cancels_registered_remote_without_late_apply() {run_native_variant(false,false,"session-abort").await;}

#[tokio::test]
async fn native_registered_compaction_matches_pinned_source_lifecycle() {run_native_variant(false,false,"lifecycle").await;}

#[tokio::test]
async fn native_registered_idle_summary_is_superseded_by_shutdown() {run_native_variant(false,false,"idle-shutdown").await;}

#[tokio::test]
async fn native_registered_idle_summary_is_superseded_by_model_change() {run_native_variant(false,false,"idle-model-change").await;}

#[tokio::test]
async fn native_registered_idle_retired_runner_rejects_late_apply() {run_native_variant(false,false,"idle-stale-runner").await;}

#[tokio::test]
async fn native_registered_remote_sse_persists_opaque_checkpoint() {run_native_variant(false,false,"remote-sse-success").await;}

#[tokio::test(start_paused=true)]
async fn registered_prompt_arrival_stands_down_pending_idle_retry() {
    run_idle_retry_prompt_scenario(true).await;
}

#[tokio::test(start_paused=true)]
async fn registered_idle_retry_dispatches_without_prompt_arrival() {
    run_idle_retry_prompt_scenario(false).await;
}

async fn run_idle_retry_prompt_scenario(prompt_arrives:bool) {
    use maho_ai::types::ContentBlock;
    use maho_ai::providers::faux::{register_faux_provider,RegisterFauxProviderOptions,FauxResponseStep,faux_assistant_message,FauxAssistantMessageOptions};
    let provider=register_faux_provider(RegisterFauxProviderOptions {api:Some("compaction-pending-idle-retry".into()),tokens_per_second:Some(0.),..Default::default()});
    let model=provider.get_model(None).expect("retry model");
    let (started,mut starts)=tokio::sync::mpsc::unbounded_channel();
    let (cancelled,mut cancellations)=tokio::sync::mpsc::unbounded_channel();
    provider.set_responses(vec![maho_ai::utils::lazy::setup_error_message(&model,"upstream_stream_truncated").into(),FauxResponseStep::Factory(Arc::new(move |_,options,_,_| {
        let started=started.clone();let cancelled=cancelled.clone();let signal=options.and_then(|options|options.stream.request.signal.clone()).expect("retry signal");
        Box::pin(async move {started.send(()).expect("retry request subscription");signal.cancelled().await;cancelled.send(()).expect("retry cleanup subscription");faux_assistant_message(Vec::<ContentBlock>::new(),FauxAssistantMessageOptions {stop_reason:Some(maho_ai::types::StopReason::Aborted),..Default::default()})})
    }))]);
    let (probe,mut probes)=tokio::sync::mpsc::unbounded_channel();
    let mut settings=policy_settings();settings.idle_compaction_enabled=true;settings.keep_recent_tokens=1;
    let actions=Arc::new(PolicyActions {settings:std::sync::Mutex::new(settings),usage:Some(ContextUsage {tokens:Some(55000),context_window:100000,percent:Some(55.)}),idle_probe:Some(probe)});
    let branch=vec![SessionEntry {id:"old".into(),parent_id:None,timestamp:String::new(),kind:"message".into(),data:serde_json::json!({"message":{"role":"user","content":"old ".repeat(2000),"timestamp":0}})},SessionEntry {id:"keep".into(),parent_id:Some("old".into()),timestamp:String::new(),kind:"message".into(),data:serde_json::json!({"message":{"role":"user","content":"next","timestamp":0}})}];
    let mut ctx=context();ctx.mode=ExtensionMode::Rpc;ctx.model=Some(model);ctx.session_manager=Arc::new(AuthSession {actions,branch});
    let mut api=ExtensionApi::new(LoadedExtension::new("compaction","/tmp".into(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),ExtensionRuntime::default());
    maho_ext_compaction::CompactionExtension.register(&mut api);
    let mut end=ExtensionEvent::AgentEnd {messages:Vec::new(),aborted:Some(false),will_retry:Some(false),abort_source:None};
    for handler in &api.registered.handlers[&EventKind::AgentEnd] {handler(&mut end,&ctx).await.expect("registered idle hook");}
    let scheduled=tokio::time::timeout(std::time::Duration::from_secs(10),probes.recv()).await;
    if matches!(scheduled,Ok(Some(()))) && prompt_arrives {
        let mut before=ExtensionEvent::BeforeAgentStart(BeforeAgentStartEvent {prompt:"next".into(),images:None,system_prompt:"base".into(),system_prompt_options:Default::default()});
        for handler in &api.registered.handlers[&EventKind::BeforeAgentStart] {handler(&mut before,&ctx).await.expect("registered prompt hook");}
    }
    tokio::time::advance(std::time::Duration::from_millis(maho_ext_compaction::idle_retry::IDLE_WARMUP_RETRY_DELAY_MS+1)).await;
    let retried=if prompt_arrives {None} else {Some(tokio::time::timeout(std::time::Duration::from_secs(10),starts.recv()).await)};
    let mut shutdown=ExtensionEvent::SessionShutdown(SessionShutdownEvent {reason:SessionReason::Quit,target_session_file:None,signal:None});
    for handler in &api.registered.handlers[&EventKind::SessionShutdown] {handler(&mut shutdown,&ctx).await.expect("retry cleanup");}
    let cleanup=if prompt_arrives {None} else {Some(tokio::time::timeout(std::time::Duration::from_secs(10),cancellations.recv()).await)};
    let calls=provider.get_call_log().len();provider.unregister();
    if let Some(cleanup)=cleanup {assert!(matches!(cleanup,Ok(Some(()))),"actual retry provider cancellation: {cleanup:?}");}
    assert!(matches!(scheduled,Ok(Some(()))),"watcher evaluates retry eligibility before prompt: {scheduled:?}");
    if let Some(retried)=retried {assert!(matches!(retried,Ok(Some(()))),"positive control retry dispatch: {retried:?}");assert_eq!(calls,2);}
    else {assert_eq!(calls,1,"prompt arrival cancels pending idle retry");assert!(starts.try_recv().is_err());}
    println!("PASS idle-prompt-retry: prompt_arrives={prompt_arrives}; subscribed retry eligibility, virtual deadline, registered cleanup");
}

#[tokio::test]
async fn registered_remote_model_auth_routes_only_to_refreshed_endpoint() {
    run_model_auth_capture(false).await;
}

#[tokio::test]
async fn registered_remote_model_auth_stream_consumes_extra_body() {
    run_model_auth_capture(true).await;
}

#[tokio::test]
async fn registered_remote_fallback_payloads_match_source_branches() {
    run_fallback_event_capture().await;
}

async fn run_fallback_event_capture() {
    for reason in ["branch-compaction","not-openai-responses","auth lookup failed","empty-compaction-input","missing-openai-auth","missing-remote-replay-origin-provenance"] {
        let model:Model=serde_json::from_value(serde_json::json!({"id":"context-model","name":"m","api":if reason=="not-openai-responses" {"faux"} else {"openai-responses"},"provider":"openai","baseUrl":"http://127.0.0.1:1/v1","reasoning":false,"input":["text"],"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":100000,"maxTokens":1000})).expect("fallback model");
        let branch=if reason=="empty-compaction-input" {Vec::new()} else {vec![SessionEntry {id:"old".into(),parent_id:None,timestamp:String::new(),kind:"message".into(),data:serde_json::json!({"type":"message","id":"old","message":{"role":"user","content":"old","timestamp":0}})}]};
        let mut ctx=context();ctx.model=Some(model);
        ctx.model_registry=Arc::new(FallbackRegistry(if reason=="auth lookup failed" {None} else {Some(ResolvedRequestAuth {auth:maho_ai::models::ProviderAuthResult {api_key:if reason=="missing-openai-auth" {None} else {Some("fake".into())},headers:None,base_url:if reason=="missing-remote-replay-origin-provenance" {Some("invalid url".into())} else {None}},extra_body:None,upstream_model_id:Some("request-model".into()),service_tier:None,env:None})}));
        ctx.session_manager=Arc::new(AuthSession {actions:Arc::new(PolicyActions::new(policy_settings())),branch});
        let captured=Arc::new(std::sync::Mutex::new(Vec::new()));let events=EventBus::default();let observed=Arc::clone(&captured);
        let _subscription=events.on(maho_ext_compaction::openai_remote::SENPI_COMPACTION_EVENT,Arc::new(move |data|observed.lock().expect("captured events").push(data.clone())));
        let runtime=ExtensionRuntime::default();runtime.bind_session_actions(Arc::new(AuthCheckpointActions));
        let mut api=ExtensionApi::new(LoadedExtension::new("compaction","/tmp".into(),SourceInfo::default()),ExtensionSessionProfile::default(),events,runtime);
        maho_ext_compaction::CompactionExtension.register(&mut api);
        let mut event=maho_ext_compaction::extension_wiring::create_live_blocking_remote_compaction_event(&ctx,CompactionPreparation {first_kept_entry_id:"keep".into(),messages_to_summarize:Vec::new(),turn_prefix_messages:Vec::new(),tokens_before:10,previous_summary:None,settings:CompactionSettings {enabled:true,reserve_tokens:1,keep_recent_tokens:1}},String::new(),AbortSignal::default());
        event.request_id="fallback-request".into();if reason=="branch-compaction" {event.reason=CompactionReason::Branch;}
        let mut event=ExtensionEvent::SessionBeforeCompact(event);
        let result=tokio::time::timeout(std::time::Duration::from_secs(10),async {for handler in &api.registered.handlers[&EventKind::SessionBeforeCompact] {handler(&mut event,&ctx).await?;}Ok::<_,ExtensionFailure>(())}).await.expect("bounded fallback handler");
        assert_eq!(result.expect_err("source fallback reaches local generator").message,"local fallback reached");
        assert_eq!(*captured.lock().expect("captured fallback"),vec![serde_json::json!({"version":1,"action":"remote_fallback","route":"builtin.compaction.openai_remote","requestId":"fallback-request","modelId":if reason=="missing-remote-replay-origin-provenance" {"request-model"} else {"context-model"},"reason":reason})]);
    }
}

async fn run_model_auth_capture(sse:bool) {
    use tokio::io::{AsyncReadExt,AsyncWriteExt};
    let stale=std::net::TcpListener::bind("127.0.0.1:0").expect("stale listener");
    stale.set_nonblocking(true).expect("stale nonblocking probe");
    let live=tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("live listener");
    let live_base=format!("http://{}/v1",live.local_addr().expect("live address"));
    let mut model:Model=serde_json::from_value(serde_json::json!({"id":"stale-model","name":"m","api":"openai-responses","provider":"openai","baseUrl":format!("http://{}/v1",stale.local_addr().expect("stale address")),"reasoning":false,"input":["text"],"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":100000,"maxTokens":1000})).expect("auth model");
    if sse {
        model.compat=Some(maho_ai::types::ModelCompat(serde_json::Map::from_iter([("supportsRemoteCompactionV2".into(),serde_json::json!(true)),("supportsWebSocket".into(),serde_json::json!(false))])));
        maho_ai::api_registry::register_builtin_api_provider("openai-responses",Arc::new(NativeResponsesStreams));
    }
    let server=tokio::spawn(async move {
        let mut captures=Vec::new();
        for attempt in 0..if sse {2} else {1} {
        let (mut socket,_)=live.accept().await.expect("live connection");
        let mut bytes=Vec::new();let mut chunk=[0;4096];
        let (header,body)=loop {
            let n=socket.read(&mut chunk).await.expect("request read");assert!(n>0);bytes.extend_from_slice(&chunk[..n]);
            if let Some(end)=bytes.windows(4).position(|part|part==b"\r\n\r\n") {
                let header=String::from_utf8(bytes[..end].to_vec()).expect("headers");
                let length:usize=header.lines().find_map(|line|line.to_lowercase().strip_prefix("content-length:").map(|value|value.trim().parse().expect("length"))).expect("length header");
                if bytes.len()>=end+4+length {break (header,serde_json::from_slice::<serde_json::Value>(&bytes[end+4..end+4+length]).expect("body"));}
            }
        };
        let (content_type,response)=if sse && attempt==0 {
            ("text/event-stream","data: {\"type\":\"response.completed\",\"response\":{\"id\":\"empty\",\"output\":[],\"usage\":{\"input_tokens\":1,\"output_tokens\":0}}}\n\ndata: [DONE]\n\n".into())
        } else {("application/json",serde_json::json!({"id":"auth-compact","object":"response.compaction","created_at":0,"output":[{"type":"compaction","encrypted_content":"opaque"}]}).to_string())};
        socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",response.len()).as_bytes()).await.expect("response");
        captures.push((header,body));
        }
        captures
    });
    let branch=vec![SessionEntry {id:"old".into(),parent_id:None,timestamp:String::new(),kind:"message".into(),data:serde_json::json!({"type":"message","id":"old","message":{"role":"user","content":"old context","timestamp":0}})}];
    let mut ctx=context();ctx.model=Some(model.clone());ctx.service_tier=Some(ServiceTier::Priority);
    ctx.model_registry=Arc::new(AuthRegistry {model,auth:ResolvedRequestAuth {auth:maho_ai::models::ProviderAuthResult {api_key:Some("refreshed-key".into()),headers:Some(std::collections::BTreeMap::from([("x-refreshed".into(),Some("live".into()))])),base_url:Some(live_base.clone())},upstream_model_id:Some("live-model".into()),extra_body:Some(serde_json::Map::from_iter([("stream_extra".into(),serde_json::json!(true))])),service_tier:Some(maho_ai::types::ServiceTierPreference::Flex),env:None}});
    ctx.session_manager=Arc::new(AuthSession {actions:Arc::new(PolicyActions::new(policy_settings())),branch});
    let registered=LoadedExtension::new("compaction","/tmp".into(),SourceInfo::default());
    let runtime=ExtensionRuntime::default();runtime.bind_session_actions(Arc::new(AuthCheckpointActions));
    let mut api=ExtensionApi::new(registered,ExtensionSessionProfile::default(),EventBus::default(),runtime);
    maho_ext_compaction::CompactionExtension.register(&mut api);
    let mut event=ExtensionEvent::SessionBeforeCompact(maho_ext_compaction::extension_wiring::create_live_blocking_remote_compaction_event(&ctx,CompactionPreparation {first_kept_entry_id:"keep".into(),messages_to_summarize:Vec::new(),turn_prefix_messages:Vec::new(),tokens_before:100,previous_summary:None,settings:CompactionSettings {enabled:true,reserve_tokens:1,keep_recent_tokens:1}},String::new(),AbortSignal::default()));
    let outcome=tokio::time::timeout(std::time::Duration::from_secs(10),async {
        let mut result=None;
        for handler in &api.registered.handlers[&EventKind::SessionBeforeCompact] {
            if let EventResult::SessionBefore(before)=handler(&mut event,&ctx).await? && before.compaction.is_some() {result=before.compaction;}
        }
        Ok::<_,ExtensionFailure>(result)
    }).await;
    let mut server=server;
    let capture=tokio::time::timeout(std::time::Duration::from_secs(10),&mut server).await;
    if capture.is_err() {server.abort();let _=server.await;}
    assert!(matches!(&outcome,Ok(Ok(Some(_)))),"registered outcome before capture: {outcome:?}");
    let captures=capture.expect("bounded capture").expect("server result");
    assert_eq!(captures.len(),if sse {2} else {1});
    for (index,(header,body)) in captures.iter().enumerate() {
        let stream=sse && index==0;
        assert!(header.starts_with(if stream {"POST /v1/responses "} else {"POST /v1/responses/compact "}));assert!(header.contains("Bearer refreshed-key"));assert!(header.contains("x-refreshed: live"));
        assert_eq!(body["model"],"live-model");
        if stream {assert!(body.get("service_tier").is_none(),"pinned Responses v2 options do not project compact request tier");} else {assert_eq!(body["service_tier"],"priority");}
        if stream {assert_eq!(body["stream_extra"],true);} else {assert!(body.get("stream_extra").is_none(),"source extraBody is stream-only");}
    }
    let result=outcome.expect("bounded registered handler").expect("handler result").expect("remote compaction");
    assert_eq!(result.details.expect("details")["origin"]["endpoint"],live_base);
    assert!(matches!(stale.accept(),Err(error) if error.kind()==std::io::ErrorKind::WouldBlock),"stale endpoint must receive zero requests");
}

struct CancellationObserver(Arc<std::sync::Mutex<Option<AbortSignal>>>);
struct IdleContextObserver(Arc<std::sync::Mutex<Option<ExtensionContext>>>);
impl Extension for IdleContextObserver {
    fn register(&self,api:&mut ExtensionApi) {
        let captured=Arc::clone(&self.0);
        api.on(EventKind::AgentEnd,Arc::new(move |_,context| {
            *captured.lock().expect("idle context capture")=Some(context.clone());
            Box::pin(async {Ok(EventResult::None)})
        }));
    }
}
struct LiveFractionalObserver;
impl Extension for LiveFractionalObserver {
    fn register(&self,api:&mut ExtensionApi) {
        api.on(EventKind::SessionBeforeCompact,Arc::new(|_,ctx| {
            let actual=ctx.get_resolved_compaction_settings().expect("bound settings").expect("real core companion");
            let mut expected=policy_settings();
            expected.keep_recent_tokens=1;expected.speculative_cooldown_ms=321.5;
            expected.restoration_max_items=2.5;expected.restoration_max_tokens_per_item=11.5;expected.restoration_max_total_tokens=22.5;
            expected.restoration_context_ratio=0.03;expected.speculative_lead_tokens=Some(12000.5);expected.summarization_max_duration_ms=Some(90000.5);
            assert_eq!(actual,expected,"real session must project all supplied fields");
            let mapped=maho_ext_compaction::extension_wiring::resolved_settings(&ctx.get_compaction_settings().expect("legacy settings"),Some(&actual)).expect("fractional builtin consumption");
            assert_eq!(mapped.speculative_cooldown_ms,Some(321.5));
            assert_eq!(mapped.ideal.speculative_lead_tokens,Some(12000.5));
            Box::pin(async {Ok(EventResult::None)})
        }));
    }
}

#[tokio::test]
async fn native_real_session_consumes_all_nondefault_fractional_settings() {run_native_variant(false,false,"fractional").await;}

#[tokio::test]
async fn native_repeated_requests_do_not_repeat_reminder_lease() {run_native_variant(false,false,"reminder").await;}

struct NativeResponsesStreams;
impl maho_ai::types::ProviderStreams for NativeResponsesStreams {
    fn stream(&self,model:&Model,context:&maho_ai::types::Context,options:Option<maho_ai::types::StreamOptions>)->maho_ai::types::AssistantMessageEventStream {
        maho_ai::api::openai_responses::stream(model,context,options)
    }
    fn stream_simple(&self,model:&Model,context:&maho_ai::types::Context,options:Option<maho_ai::types::SimpleStreamOptions>)->maho_ai::types::AssistantMessageEventStream {
        maho_ai::api::openai_responses::stream_simple(model,context,options)
    }
}
impl Extension for CancellationObserver {
    fn register(&self,api:&mut ExtensionApi) {
        let observed=Arc::clone(&self.0);
        api.on(EventKind::SessionBeforeCompact,Arc::new(move |event,_| {
            if let ExtensionEvent::SessionBeforeCompact(event)=event {*observed.lock().expect("native compaction scenario invariant")=Some(event.signal.clone());}
            Box::pin(async {Ok(EventResult::None)})
        }));
    }
}

#[tokio::test]
async fn live_warm_claim_rejects_model_and_prefix_supersession() {
    use maho_ext_compaction::{speculative::SpeculativeCompactionSnapshot,speculative_job::{track_speculative_job,JobSettlement,LiveSummaryFailure,claim_live_warm_job}};
    let model:Model=serde_json::from_value(serde_json::json!({"id":"m","name":"m","api":"faux","provider":"faux","baseUrl":"","reasoning":false,"input":["text"],"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":100000,"maxTokens":1000})).expect("warm model");
    let entries=vec![serde_json::json!({"type":"message","id":"old","parentId":null,"timestamp":"1970-01-01T00:00:00.000Z","message":{"role":"user","content":"old ".repeat(1000),"timestamp":0}}),serde_json::json!({"type":"message","id":"keep","parentId":"old","timestamp":"1970-01-01T00:00:00.000Z","message":{"role":"user","content":"continue","timestamp":0}})];
    let mut settings=maho_core::compaction::settings::default_compaction_settings();settings.keep_recent_tokens=1;
    let preparation=maho_core::compaction::compaction::prepare_compaction(&entries,&settings,true,false).expect("warm preparation");
    let snapshot=SpeculativeCompactionSnapshot {generation:1,expected_revision:0,model:model.clone(),context_window:100000,preparation:preparation.clone(),branch_entries:entries.clone(),prompt_variant:maho_ext_compaction::prompts::PromptVariant::Default,custom_instructions:None,system_prompt:None,tools:Vec::new(),origin:Some("speculative".into())};
    let mut job=Some(track_speculative_job(1,snapshot,maho_ai::utils::abort::AbortController::new(),async {JobSettlement::<CompactionResult,LiveSummaryFailure> {result:None,error:None}},0));
    let branch:Vec<SessionEntry>=entries.into_iter().map(|entry|SessionEntry {id:entry["id"].as_str().expect("entry id").into(),parent_id:entry["parentId"].as_str().map(str::to_owned),timestamp:String::new(),kind:"message".into(),data:entry}).collect();
    let mut ctx=context();ctx.model=Some(model.clone());
    let mut event=maho_ext_compaction::extension_wiring::create_live_blocking_remote_compaction_event(&ctx,CompactionPreparation {first_kept_entry_id:preparation.first_kept_entry_id,messages_to_summarize:Vec::new(),turn_prefix_messages:Vec::new(),tokens_before:0,previous_summary:None,settings:CompactionSettings {enabled:true,reserve_tokens:1,keep_recent_tokens:1}},String::new(),AbortSignal::default());
    event.custom_instructions=None;event.branch_entries=branch;
    ctx.model.as_mut().expect("selected model").id="other".into();
    assert!(claim_live_warm_job(&mut job,&event,&ctx).is_none());assert!(job.is_some());
    ctx.model=Some(model);event.branch_entries[0].id="changed".into();
    assert!(claim_live_warm_job(&mut job,&event,&ctx).is_none());assert!(job.is_some());
    event.branch_entries[0].id="old".into();
    let claimed=claim_live_warm_job(&mut job,&event,&ctx).expect("matching model and anchor claim");assert!(job.is_none());
    tokio::time::timeout(std::time::Duration::from_secs(10),claimed.settled()).await.expect("warm task cleanup");
}

fn maho_agent_for_test(model: Model) -> maho_agent::Agent {
    maho_agent::Agent::new(maho_agent::AgentOptions { initial_state: Some(maho_agent::agent::PartialAgentState { model: Some(model), ..Default::default() }),
        stream_fn: Some(Arc::new(|model, context, options|maho_ai::stream::stream_simple(model,context,options.map(|options|options.simple)))), ..Default::default() })
}
#[tokio::test]
async fn registered_lifecycle_handlers_accept_real_api_events() {
    let registered = LoadedExtension::new("compaction", "/tmp".into(), SourceInfo::default());
    let mut api = ExtensionApi::new(registered, ExtensionSessionProfile::default(), EventBus::default(), ExtensionRuntime::default());
    maho_ext_compaction::CompactionExtension.register(&mut api);
    let ctx = context();
    let actions = Arc::new(PolicyActions::new(policy_settings()));
    let mut ctx = ctx;
    ctx.session_manager = Arc::new(PolicySession(actions));
    let mut event = ExtensionEvent::SessionCompact(SessionCompactEvent::Accepted {
        reason: CompactionReason::Extension, request_id: "request".into(),
        compaction_entry: SessionEntry { id: "compact".into(), parent_id: None, timestamp: "date".into(), kind: "compaction".into(), data: serde_json::json!({"tokensBefore":10000,"details":{"structuralYield":{"savedTokens":4000,"savingsRatio":0.4}}}) },
        from_extension: true, will_retry: false,
    });
    for handler in &api.registered.handlers[&EventKind::SessionCompact] {
        assert!(matches!(handler(&mut event, &ctx).await.expect("native compaction scenario invariant"), EventResult::None));
    }
    let mut event = ExtensionEvent::AgentEnd { messages: Vec::new(), aborted: None, will_retry: None, abort_source: None };
    for handler in &api.registered.handlers[&EventKind::AgentEnd] {
        assert!(matches!(handler(&mut event, &ctx).await.expect("native compaction scenario invariant"), EventResult::None));
    }
}

#[tokio::test]
async fn preaborted_compaction_does_not_read_unbound_checkpoint_actions() {
    let registered = LoadedExtension::new("compaction", "/tmp".into(), SourceInfo::default());
    let mut api = ExtensionApi::new(registered, ExtensionSessionProfile::default(), EventBus::default(), ExtensionRuntime::default());
    maho_ext_compaction::CompactionExtension.register(&mut api);
    let signal = AbortSignal::default();
    signal.abort();
    let mut event = ExtensionEvent::SessionBeforeCompact(SessionBeforeCompactEvent {
        reason: CompactionReason::Manual, will_retry: false, request_id: "cancelled".into(),
        preparation: CompactionPreparation { settings: CompactionSettings { enabled: true, reserve_tokens: 100, keep_recent_tokens: 100 },
            messages_to_summarize: Vec::new(), turn_prefix_messages: Vec::new(), tokens_before: 1000, first_kept_entry_id: "keep".into(), previous_summary: None },
        branch_entries: Vec::new(), custom_instructions: None, signal,
    });
    for handler in &api.registered.handlers[&EventKind::SessionBeforeCompact] {
        assert!(matches!(handler(&mut event, &context()).await.expect("native compaction scenario invariant"), EventResult::None));
    }
}

#[tokio::test]
async fn rejected_compactions_trip_registered_breaker_but_external_owner_does_not() {
    for cause in [CompactionRejectionCause::CancelledByExtension, CompactionRejectionCause::ExternalOwner] {
        let mut api = ExtensionApi::new(LoadedExtension::new("compaction", "/tmp".into(), SourceInfo::default()), ExtensionSessionProfile::default(), EventBus::default(), ExtensionRuntime::default());
        maho_ext_compaction::CompactionExtension.register(&mut api);
        let ctx = context();
        for _ in 0..3 {
            let mut event = ExtensionEvent::SessionCompact(SessionCompactEvent::Rejected { reason: CompactionReason::Threshold, request_id: "request".into(), rejection_cause: cause });
            for handler in &api.registered.handlers[&EventKind::SessionCompact] { handler(&mut event,&ctx).await.expect("native compaction scenario invariant"); }
        }
        let mut event = ExtensionEvent::SessionBeforeCompact(SessionBeforeCompactEvent {
            reason: CompactionReason::Threshold, will_retry: false, request_id: "request".into(),
            preparation: CompactionPreparation { settings: CompactionSettings { enabled: true, reserve_tokens: 100, keep_recent_tokens: 100 }, messages_to_summarize: Vec::new(), turn_prefix_messages: Vec::new(), tokens_before: 1000, first_kept_entry_id: "keep".into(), previous_summary: None },
            branch_entries: Vec::new(), custom_instructions: None, signal: AbortSignal::default(),
        });
        let result = api.registered.handlers[&EventKind::SessionBeforeCompact][0](&mut event,&ctx).await;
        if cause == CompactionRejectionCause::ExternalOwner { assert!(result.is_err()); }
        else { assert!(matches!(result.expect("native compaction scenario invariant"), EventResult::SessionBefore(SessionBeforeEventResult { rejection_cause: Some(CompactionRejectionCause::CircuitBreaker), .. }))); }
    }
}

#[test]
fn disabled_restoration_and_rejected_compaction_never_read_unbound_context() {
    let mut settings = maho_core::compaction::settings::default_compaction_settings();
    settings.restoration_enabled = Some(false);
    let mut state = maho_ext_compaction::restoration_tracker::RestorationTrackerState::default();
    let accepted = SessionCompactEvent::Accepted { reason: CompactionReason::Manual, request_id: "r".into(), compaction_entry: SessionEntry { id:"c".into(),parent_id:None,timestamp:String::new(),kind:"compaction".into(),data:serde_json::json!({}) },from_extension:true,will_retry:false };
    maho_ext_compaction::extension_wiring::prepare_accepted_restoration(&mut state,&context(),&accepted,&settings).expect("native compaction scenario invariant");
    settings.restoration_enabled = Some(true);
    let rejected = SessionCompactEvent::Rejected { reason:CompactionReason::Manual,request_id:"r".into(),rejection_cause:CompactionRejectionCause::ExternalOwner };
    maho_ext_compaction::extension_wiring::prepare_accepted_restoration(&mut state,&context(),&rejected,&settings).expect("native compaction scenario invariant");
    assert!(state.pending_payload.is_none());
}

