use maho_server::app_server::{envelope::classify_incoming,runtime::AppServerRuntime,thread_registry::SessionFactory};
use serde_json::{Value,json};
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::Mutex;

fn factory() -> SessionFactory {
    Arc::new(|options| Box::pin(async move {
        use maho_ai::providers::faux::{FauxAssistantMessageOptions, RegisterFauxProviderOptions, faux_assistant_message, faux_provider, faux_streams};
        use maho_core::agent_session::{AgentSession, AgentSessionConfig};
        let cwd = options.cwd.unwrap_or_default();
        let provider = faux_provider(RegisterFauxProviderOptions {api:Some("faux".into()),models:Some(vec![maho_ai::providers::faux::FauxModelDefinition {id:"faux-1".into(),reasoning:Some(true),..Default::default()}]),tokens_per_second:Some(0.0),..Default::default()});
        let model = provider.get_model(Some("faux-1")).ok_or("Missing faux model")?;
        provider.set_responses(vec![faux_assistant_message("retained transcript",FauxAssistantMessageOptions {timestamp:Some(0),..Default::default()}).into()]);
        let streams = faux_streams(provider.core.clone());
        let stream_fn: maho_agent::types::StreamFn = Arc::new(move |model, context, options| streams.stream_simple(model,context,options.map(|options|options.simple)));
        let credentials=Arc::new(maho_core::auth_storage::AuthStorage::in_memory([("faux".into(),json!({"type":"api_key","key":"faux-test"}))].into_iter().collect()));
        let runtime = maho_core::model_runtime::ModelRuntime::create_sync(maho_core::model_runtime::CreateModelRuntimeOptions {credentials:Some(credentials),models_path:Some(std::path::Path::new(&cwd).join("models.json")),auth_path:Some(std::path::Path::new(&cwd).join("auth.json")),providers:Some(vec![provider.provider.clone()])});
        AgentSession::new(AgentSessionConfig {
            agent:maho_agent::Agent::new(maho_agent::AgentOptions {initial_state:Some(maho_agent::agent::PartialAgentState {model:Some(model),..Default::default()}),stream_fn:Some(stream_fn),..Default::default()}),
            session_manager:options.session_manager.ok_or("Missing session manager")?,settings_manager:maho_core::settings_manager::SettingsManager::from_storage(Box::new(maho_core::settings_manager::InMemorySettingsStorage::default()),false),
            cwd:cwd.clone(),agent_dir:Some(cwd),fallback_now:Some(Arc::new(||0.0)),retry_random:Some(Arc::new(||0.5)),scoped_models:Vec::new(),favorite_models:Vec::new(),flag_values:Default::default(),custom_tools:Vec::new(),model_runtime:Some(runtime),model_registry:None,uses_default_stream_function:Some(false),initial_active_tool_names:None,default_tool_names:None,eval_only_tool_names:None,allowed_tool_names:None,excluded_tool_names:None,base_tools_override:None,session_start_event:None,auto_title_sessions:Some(false),
        }).map_err(|error|error.to_string())
    }))
}

struct Harness {
    runtime: AppServerRuntime,
    receive: tokio::sync::mpsc::UnboundedReceiver<Value>,
    notifications: VecDeque<Value>,
    _directory: tempfile::TempDir,
}
impl Harness {
    async fn call(&mut self, request: Value) -> Value {
        let id = request["id"].clone();
        self.runtime.core.read().await.receive("qa", classify_incoming(request)).await.unwrap();
        loop {
            let message = tokio::time::timeout(std::time::Duration::from_secs(5), self.receive.recv()).await.expect("response within deadline").expect("connection stays open");
            if message.get("id").is_some_and(|value| *value == id) { return message; }
            self.notifications.push_back(message);
        }
    }
    async fn notification(&mut self) -> Value {
        if let Some(notification) = self.notifications.pop_front() { return notification; }
        tokio::time::timeout(std::time::Duration::from_secs(5), self.receive.recv()).await.expect("notification within deadline").expect("connection stays open")
    }
    fn recorded_before_response(&self) -> usize { self.notifications.len() }
    async fn start_thread(&mut self) -> String {
        let response = self.call(json!({"id":2,"method":"thread/start","params":{}})).await;
        assert_eq!(self.recorded_before_response(),0,"response must precede thread/started");
        assert_eq!(self.notification().await["method"],"thread/started");
        response["result"]["thread"]["id"].as_str().unwrap().to_owned()
    }
    async fn call_notifying(&mut self, request: Value, method: &str) -> (Value, Value) {
        let response = self.call(request).await;
        assert_eq!(self.recorded_before_response(),0,"response must precede {method}");
        let notification = self.notification().await;
        assert_eq!(notification["method"],method);
        (response, notification)
    }
}
async fn harness() -> Harness {
    let directory = tempfile::tempdir().unwrap();
    let runtime = AppServerRuntime::new(directory.path().join("agent").display().to_string(),directory.path().display().to_string(),"1".into(),Some(directory.path().join("sessions").display().to_string()),Some(factory())).await;
    let (send,receive) = tokio::sync::mpsc::unbounded_channel();
    runtime.core.write().await.add_connection("qa".into(),Arc::new(move |message| {send.send(message).unwrap();Box::pin(async {Ok(())})}));
    runtime.core.read().await.receive("qa",classify_incoming(json!({"id":1,"method":"initialize","params":{"clientInfo":{"name":"qa","version":"1"}}}))).await.unwrap();
    Harness { runtime, receive, notifications:VecDeque::new(), _directory:directory }
}

#[tokio::test]
async fn goal_set_get_clear_roundtrip_and_response_precedes_notification() {
    let mut harness = harness().await;
    let thread = harness.start_thread().await;
    let (response, notification) = harness.call_notifying(json!({"id":3,"method":"thread/goal/set","params":{"threadId":thread,"objective":"Ship it"}}),"thread/goal/updated").await;
    assert_eq!(response["id"],3);
    assert_eq!(response["result"]["goal"]["objective"],"Ship it");
    assert_eq!(response["result"]["goal"]["status"],"active");
    assert_eq!(response["result"]["goal"]["tokenBudget"],Value::Null);
    assert_eq!(notification["params"]["turnId"],Value::Null);
    assert_eq!(notification["params"]["goal"]["objective"],"Ship it");
    let read = harness.call(json!({"id":4,"method":"thread/goal/get","params":{"threadId":thread}})).await;
    assert_eq!(read["result"]["goal"]["objective"],"Ship it");
    let (cleared, _) = harness.call_notifying(json!({"id":5,"method":"thread/goal/clear","params":{"threadId":thread}}),"thread/goal/cleared").await;
    assert_eq!(cleared["result"]["cleared"],true);
    let empty = harness.call(json!({"id":6,"method":"thread/goal/get","params":{"threadId":thread}})).await;
    assert_eq!(empty["result"]["goal"],Value::Null);
    assert_eq!(harness.call(json!({"id":7,"method":"thread/goal/clear","params":{"threadId":thread}})).await["result"]["cleared"],false);
    assert_eq!(harness.recorded_before_response(),0);
    harness.runtime.dispose().await;
}

#[tokio::test]
async fn goal_set_updates_status_and_budget_and_rejects_unknown_thread_without_state_change() {
    let mut harness = harness().await;
    let thread = harness.start_thread().await;
    harness.call_notifying(json!({"id":3,"method":"thread/goal/set","params":{"threadId":thread,"objective":"Tracked","tokenBudget":5000}}),"thread/goal/updated").await;
    let paused = harness.call_notifying(json!({"id":4,"method":"thread/goal/set","params":{"threadId":thread,"status":"paused"}}),"thread/goal/updated").await.0;
    assert_eq!(paused["result"]["goal"]["status"],"paused");
    assert_eq!(paused["result"]["goal"]["tokenBudget"],json!(5000.0));
    assert_eq!(harness.call(json!({"id":5,"method":"thread/goal/set","params":{"threadId":"missing","objective":"Nope"}})).await["error"]["code"],-32600);
    assert_eq!(harness.call(json!({"id":6,"method":"thread/goal/set","params":{"threadId":thread,"objective":"Fresh"}})).await["error"]["code"],-32600);
    assert_eq!(harness.call(json!({"id":7,"method":"thread/goal/set","params":{"threadId":thread,"status":"bogus"}})).await["error"]["code"],-32602);
    assert_eq!(harness.call(json!({"id":8,"method":"thread/goal/set","params":{"threadId":thread,"status":"blocked"}})).await["error"]["code"],-32600);
    assert_eq!(harness.recorded_before_response(),0);
    let unchanged = harness.call(json!({"id":9,"method":"thread/goal/get","params":{"threadId":thread}})).await;
    assert_eq!(unchanged["result"]["goal"]["status"],"paused");
    assert_eq!(unchanged["result"]["goal"]["objective"],"Tracked");
    harness.runtime.dispose().await;
}

#[tokio::test]
async fn invalid_status_transition_leaves_persisted_goal_unchanged() {
    let mut harness = harness().await;
    let thread = harness.start_thread().await;
    harness.call_notifying(json!({"id":3,"method":"thread/goal/set","params":{"threadId":thread,"objective":"Finish"}}),"thread/goal/updated").await;
    let completed = harness.call_notifying(json!({"id":4,"method":"thread/goal/set","params":{"threadId":thread,"status":"complete"}}),"thread/goal/updated").await.0;
    assert_eq!(completed["result"]["goal"]["status"],"complete");
    let rejected = harness.call(json!({"id":5,"method":"thread/goal/set","params":{"threadId":thread,"status":"paused"}})).await;
    assert_eq!(rejected["error"]["code"],-32603);
    assert_eq!(harness.recorded_before_response(),0);
    let read = harness.call(json!({"id":6,"method":"thread/goal/get","params":{"threadId":thread}})).await;
    assert_eq!(read["result"]["goal"]["status"],"complete");
    assert_eq!(read["result"]["goal"]["objective"],"Finish");
    harness.runtime.dispose().await;
}

/// UB1 causal regression: the runtime registers a per-thread MCP wire-status inventory holder on
/// `thread/start` (post-response, via `bind_mcp_wire_status` -> `register_thread`), observable through
/// the registry completion signal taken BEFORE the trigger (no sleep/poll).
#[tokio::test]
async fn runtime_registers_a_per_thread_mcp_inventory_on_thread_start() {
    let mut harness = harness().await;
    let registration = harness.runtime.mcp_inventory.lock().await.ready_signal();
    let thread = harness.start_thread().await;
    tokio::time::timeout(std::time::Duration::from_secs(5), registration.notified())
        .await
        .expect("per-thread MCP inventory registration within deadline");
    let inventory = harness.runtime.mcp_inventory.lock().await;
    assert!(inventory.registration_count() >= 1, "the runtime must register a per-thread MCP inventory holder on thread start");
    assert!(inventory.resolve(Some(&thread)).is_some(), "the started thread must resolve to its registered holder");
    assert!(inventory.resolve(None).is_some(), "the process-global holder remains available");
    drop(inventory);
    harness.runtime.dispose().await;
}
/// A stateless UI whose editor text does not round-trip, so a stateful AppServerUiContext bound by
/// the runtime is distinguishable from this initial UI (guards against a false positive).
struct HeadlessUi;
impl maho_ext_api::ExtensionUi for HeadlessUi {
    fn select<'a>(&'a self,_:&'a str,_:&'a [String],_:maho_ext_api::ExtensionUiDialogOptions)->maho_ext_api::UiFuture<'a,Option<String>> {Box::pin(async {None})}
    fn confirm<'a>(&'a self,_:&'a str,_:&'a str,_:maho_ext_api::ExtensionUiDialogOptions)->maho_ext_api::UiFuture<'a,bool> {Box::pin(async {false})}
    fn input<'a>(&'a self,_:&'a str,_:Option<&'a str>,_:maho_ext_api::ExtensionUiDialogOptions)->maho_ext_api::UiFuture<'a,Option<String>> {Box::pin(async {None})}
    fn notify(&self,_:&str,_:maho_ext_api::NotificationType) {}
    fn set_status(&self,_:&str,_:Option<&str>) {}
    fn set_widget(&self,_:&str,_:Option<maho_ext_api::WidgetContent>,_:maho_ext_api::ExtensionWidgetOptions) {}
    fn set_header(&self,_:Option<maho_ext_api::ComponentFactory>) {}
    fn set_footer(&self,_:Option<maho_ext_api::ComponentFactory>) {}
    fn set_title(&self,_:&str) {}
    fn paste_to_editor(&self,_:&str) {}
    fn set_editor_text(&self,_:&str) {}
    fn get_editor_text(&self)->String {String::new()}
    fn custom(&self,_:maho_ext_api::ComponentFactory,_:maho_ext_api::CustomUiOptions)->maho_ext_api::ExtensionFuture<'_,maho_ext_api::JsonValue> {Box::pin(async {Err(maho_ext_api::ExtensionFailure::new("UI unavailable"))})}
    fn theme(&self)->maho_ext_api::Theme {maho_ext_api::Theme::default()}
}

/// A probe extension exposing the runner's currently-bound UI through two slash commands: `probe_ui_write`
/// sets the UI editor text, `probe_ui_read` records the UI editor text into `captured`. A stateful
/// bound UI (AppServerUiContext) round-trips; the stateless HeadlessUi does not. Distinct per-thread
/// values prove distinct bound UI instances (owning-thread isolation).
struct ProbeUi { captured: Arc<Mutex<Vec<String>>> }
impl maho_ext_api::Extension for ProbeUi {
    fn register(&self,api:&mut maho_ext_api::ExtensionApi) {
        api.register_command("probe_ui_write",None,None,Arc::new(move |args,ctx:&maho_ext_api::ExtensionContext| {ctx.ui.set_editor_text(args);Box::pin(async {Ok(())})}));
        let reader=self.captured.clone();
        api.register_command("probe_ui_read",None,None,Arc::new(move |_args,ctx:&maho_ext_api::ExtensionContext| {reader.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(ctx.ui.get_editor_text());Box::pin(async {Ok(())})}));
    }
}

fn probe_factory(sessions:Arc<Mutex<std::collections::BTreeMap<String,maho_core::agent_session::AgentSession>>>,captured:Arc<Mutex<Vec<String>>>)->SessionFactory {
    Arc::new(move |options| {
        let sessions=sessions.clone(); let captured=captured.clone();
        Box::pin(async move {
            use maho_ai::providers::faux::{FauxAssistantMessageOptions, RegisterFauxProviderOptions, faux_assistant_message, faux_provider, faux_streams};
            use maho_core::agent_session::{AgentSession, AgentSessionConfig, ExtensionBindings};
            let cwd = options.cwd.unwrap_or_default();
            let provider = faux_provider(RegisterFauxProviderOptions {api:Some("faux".into()),models:Some(vec![maho_ai::providers::faux::FauxModelDefinition {id:"faux-1".into(),reasoning:Some(true),..Default::default()}]),tokens_per_second:Some(0.0),..Default::default()});
            let model = provider.get_model(Some("faux-1")).ok_or("Missing faux model")?;
            provider.set_responses(vec![faux_assistant_message("retained transcript",FauxAssistantMessageOptions {timestamp:Some(0),..Default::default()}).into()]);
            let streams = faux_streams(provider.core.clone());
            let stream_fn: maho_agent::types::StreamFn = Arc::new(move |model, context, options| streams.stream_simple(model,context,options.map(|options|options.simple)));
            let credentials=Arc::new(maho_core::auth_storage::AuthStorage::in_memory([("faux".into(),json!({"type":"api_key","key":"faux-test"}))].into_iter().collect()));
            let runtime = maho_core::model_runtime::ModelRuntime::create_sync(maho_core::model_runtime::CreateModelRuntimeOptions {credentials:Some(credentials),models_path:Some(std::path::Path::new(&cwd).join("models.json")),auth_path:Some(std::path::Path::new(&cwd).join("auth.json")),providers:Some(vec![provider.provider.clone()])});
            let session = AgentSession::new(AgentSessionConfig {
                agent:maho_agent::Agent::new(maho_agent::AgentOptions {initial_state:Some(maho_agent::agent::PartialAgentState {model:Some(model),..Default::default()}),stream_fn:Some(stream_fn),..Default::default()}),
                session_manager:options.session_manager.ok_or("Missing session manager")?,settings_manager:maho_core::settings_manager::SettingsManager::from_storage(Box::new(maho_core::settings_manager::InMemorySettingsStorage::default()),false),
                cwd:cwd.clone(),agent_dir:Some(cwd),fallback_now:Some(Arc::new(||0.0)),retry_random:Some(Arc::new(||0.5)),scoped_models:Vec::new(),favorite_models:Vec::new(),flag_values:Default::default(),custom_tools:Vec::new(),model_runtime:Some(runtime),model_registry:None,uses_default_stream_function:Some(false),initial_active_tool_names:None,default_tool_names:None,eval_only_tool_names:None,allowed_tool_names:None,excluded_tool_names:None,base_tools_override:None,session_start_event:None,auto_title_sessions:Some(false),
            }).map_err(|error|error.to_string())?;
            session.set_extension_runner(maho_ext_host::runner::ExtensionRunner::from_static(vec![Box::new(ProbeUi{captured:captured.clone()})],session.extension_context(Arc::new(HeadlessUi)))).await;
            session.bind_extensions(ExtensionBindings::default()).await;
            sessions.lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(session.session_id(),session.clone());
            Ok(session)
        })
    })
}

struct ProbeHarness {
    runtime: AppServerRuntime,
    receive: tokio::sync::mpsc::UnboundedReceiver<Value>,
    sessions: Arc<Mutex<std::collections::BTreeMap<String,maho_core::agent_session::AgentSession>>>,
    captured: Arc<Mutex<Vec<String>>>,
    _directory: tempfile::TempDir,
}
impl ProbeHarness {
    async fn new() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let sessions=Arc::new(Mutex::new(std::collections::BTreeMap::new()));
        let captured=Arc::new(Mutex::new(Vec::new()));
        let runtime = AppServerRuntime::new(directory.path().join("agent").display().to_string(),directory.path().display().to_string(),"1".into(),Some(directory.path().join("sessions").display().to_string()),Some(probe_factory(sessions.clone(),captured.clone()))).await;
        let (send,receive) = tokio::sync::mpsc::unbounded_channel();
        runtime.core.write().await.add_connection("qa".into(),Arc::new(move |message| {send.send(message).unwrap();Box::pin(async {Ok(())})}));
        runtime.core.read().await.receive("qa",classify_incoming(json!({"id":1,"method":"initialize","params":{"clientInfo":{"name":"qa","version":"1"}}}))).await.unwrap();
        Self { runtime, receive, sessions, captured, _directory:directory }
    }
    async fn call(&mut self, request: Value) -> Value {
        let id = request["id"].clone();
        self.runtime.core.read().await.receive("qa", classify_incoming(request)).await.unwrap();
        loop {
            let message = tokio::time::timeout(std::time::Duration::from_secs(5), self.receive.recv()).await.expect("response within deadline").expect("connection stays open");
            if message.get("id").is_some_and(|value| *value == id) { return message; }
        }
    }
    async fn anchor(&self) -> Arc<tokio::sync::Notify> { self.runtime.mcp_inventory.lock().await.ready_signal() }
    async fn await_anchor(&self, anchor: &Arc<tokio::sync::Notify>) { tokio::time::timeout(std::time::Duration::from_secs(5), anchor.notified()).await.expect("post-rebind inventory anchor within deadline"); }
    fn session(&self, id: &str) -> maho_core::agent_session::AgentSession { self.sessions.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(id).cloned().expect("factory captured the session") }
    async fn probe_write(&self, id: &str, value: &str) {
        let disposition = self.session(id).prompt(&format!("/probe_ui_write {value}"), maho_core::agent_session::PromptOptions::default()).await.unwrap();
        assert_eq!(disposition, maho_core::agent_session::PromptDisposition::Handled);
    }
    async fn probe_read(&self, id: &str) -> String {
        let disposition = self.session(id).prompt("/probe_ui_read", maho_core::agent_session::PromptOptions::default()).await.unwrap();
        assert_eq!(disposition, maho_core::agent_session::PromptDisposition::Handled);
        self.captured.lock().unwrap_or_else(std::sync::PoisonError::into_inner).last().cloned().expect("probe recorded a read")
    }
}

/// UB1 start path: the runtime rebinds the live connection's AppServerUiContext into the started
/// thread's extension runner, observed through a real slash command reading the runner's bound UI
/// (not the session setter or the inventory count). The post-rebind inventory anchor is taken before
/// the trigger and awaited with a bounded timeout (no sleeps).
#[tokio::test]
async fn runtime_rebinds_the_connection_ui_into_the_session_runner_on_thread_start() {
    let mut harness = ProbeHarness::new().await;
    let anchor = harness.anchor().await;
    let response = harness.call(json!({"id":2,"method":"thread/start","params":{}})).await;
    let thread = response["result"]["thread"]["id"].as_str().unwrap().to_owned();
    harness.await_anchor(&anchor).await;
    harness.probe_write(&thread, "start-marker").await;
    assert_eq!(harness.probe_read(&thread).await, "start-marker", "thread/start must rebind the stateful AppServerUiContext into the session runner");
    harness.runtime.dispose().await;
}

/// UB1 resume path: a genuine non-loaded thread/resume. The thread is unloaded (registry entry + its
/// inventory holder removed, as the idle-unload lifecycle does), then thread/resume re-creates the
/// session through the factory and runs the resume block; the fresh inventory registration is the
/// post-rebind anchor, and the probe proves the runner holds the rebound UI.
#[tokio::test]
async fn runtime_rebinds_the_connection_ui_into_the_session_runner_on_thread_resume() {
    let mut harness = ProbeHarness::new().await;
    let anchor = harness.anchor().await;
    let started = harness.call(json!({"id":2,"method":"thread/start","params":{}})).await;
    let thread = started["result"]["thread"]["id"].as_str().unwrap().to_owned();
    harness.await_anchor(&anchor).await;
    // Make the thread non-loaded (its inventory holder would be removed by the idle-unload lifecycle).
    assert!(harness.runtime.threads.unload_thread(&thread).await);
    harness.runtime.mcp_inventory.lock().await.remove_thread(&thread);
    let anchor = harness.anchor().await;
    let resumed = harness.call(json!({"id":3,"method":"thread/resume","params":{"threadId":thread}})).await;
    assert_eq!(resumed["result"]["thread"]["id"].as_str(), Some(thread.as_str()));
    harness.await_anchor(&anchor).await;
    harness.probe_write(&thread, "resume-marker").await;
    assert_eq!(harness.probe_read(&thread).await, "resume-marker", "thread/resume must rebind the stateful AppServerUiContext into the session runner");
    harness.runtime.dispose().await;
}

/// UB1 fork path: thread/fork creates a new thread/runner and runs the fork block; the fresh inventory
/// registration is the post-rebind anchor and the probe proves the forked runner holds the rebound UI.
#[tokio::test]
async fn runtime_rebinds_the_connection_ui_into_the_session_runner_on_thread_fork() {
    let mut harness = ProbeHarness::new().await;
    let anchor = harness.anchor().await;
    let started = harness.call(json!({"id":2,"method":"thread/start","params":{}})).await;
    let source = started["result"]["thread"]["id"].as_str().unwrap().to_owned();
    harness.await_anchor(&anchor).await;
    let anchor = harness.anchor().await;
    let forked = harness.call(json!({"id":3,"method":"thread/fork","params":{"threadId":source}})).await;
    let forked_id = forked["result"]["thread"]["id"].as_str().unwrap().to_owned();
    assert_ne!(forked_id, source);
    harness.await_anchor(&anchor).await;
    harness.probe_write(&forked_id, "fork-marker").await;
    assert_eq!(harness.probe_read(&forked_id).await, "fork-marker", "thread/fork must rebind the stateful AppServerUiContext into the forked session runner");
    harness.runtime.dispose().await;
}

/// UB1 owning-thread isolation: two live threads must each bind a DISTINCT AppServerUiContext. Writing
/// a per-thread marker and reading it back through each thread's own runner yields that thread's own
/// marker; a shared UI would leak the other thread's last write.
#[tokio::test]
async fn runtime_binds_distinct_ui_contexts_per_owning_thread() {
    let mut harness = ProbeHarness::new().await;
    let anchor = harness.anchor().await;
    let first = harness.call(json!({"id":2,"method":"thread/start","params":{}})).await;
    let first = first["result"]["thread"]["id"].as_str().unwrap().to_owned();
    harness.await_anchor(&anchor).await;
    let anchor = harness.anchor().await;
    let second = harness.call(json!({"id":3,"method":"thread/start","params":{}})).await;
    let second = second["result"]["thread"]["id"].as_str().unwrap().to_owned();
    assert_ne!(first, second);
    harness.await_anchor(&anchor).await;
    harness.probe_write(&first, "first-thread").await;
    harness.probe_write(&second, "second-thread").await;
    assert_eq!(harness.probe_read(&first).await, "first-thread", "the first thread must keep its own bound UI context");
    assert_eq!(harness.probe_read(&second).await, "second-thread", "the second thread must keep its own bound UI context");
    harness.runtime.dispose().await;
}

/// UB1 loaded-resume path: thread/resume of an ALREADY-loaded thread reuses the session (no factory
/// call) but still runs the resume block's rebind. A pre-resume editor marker written through the
/// runner's bound UI must be gone after resume because the block binds a fresh AppServerUiContext.
/// The MCP holder is cleared first (a realistic missing-holder state) so the resume's registration is
/// the exact post-rebind anchor, avoiding sleeps.
#[tokio::test]
async fn runtime_rebinds_the_connection_ui_into_the_session_runner_on_loaded_thread_resume() {
    let mut harness = ProbeHarness::new().await;
    let anchor = harness.anchor().await;
    let started = harness.call(json!({"id":2,"method":"thread/start","params":{}})).await;
    let thread = started["result"]["thread"]["id"].as_str().unwrap().to_owned();
    harness.await_anchor(&anchor).await;
    harness.probe_write(&thread, "before-resume").await;
    assert_eq!(harness.probe_read(&thread).await, "before-resume");
    // Clear the holder (the session stays loaded) so the loaded-resume registration re-fires and anchors the rebind.
    harness.runtime.mcp_inventory.lock().await.remove_thread(&thread);
    let anchor = harness.anchor().await;
    let resumed = harness.call(json!({"id":3,"method":"thread/resume","params":{"threadId":thread}})).await;
    assert_eq!(resumed["result"]["thread"]["id"].as_str(), Some(thread.as_str()));
    harness.await_anchor(&anchor).await;
    assert_eq!(harness.probe_read(&thread).await, "", "loaded thread/resume must bind a fresh AppServerUiContext, clearing the prior editor state");
    harness.runtime.dispose().await;
}
