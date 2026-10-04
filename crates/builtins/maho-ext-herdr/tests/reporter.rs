use maho_ext_herdr::reporter::{count_running_child_tasks, has_user_reporter, Herdr, HerdrDependencies, HerdrExtension};
use maho_ext_api::*;
use serde_json::{json, Value};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixListener;

#[test]
fn reporter_registration(){let mut api=ExtensionApi::new(LoadedExtension::new("herdr","/tmp".into(),SourceInfo::default()),ExtensionSessionProfile::default(),EventBus::default(),ExtensionRuntime::default());Herdr.register(&mut api);for kind in [EventKind::SessionStart,EventKind::SessionInfoChanged,EventKind::AgentStart,EventKind::AgentSettled,EventKind::SessionShutdown]{assert_eq!(api.registered.handlers[&kind].len(),1);}}
#[test]
fn managed_and_user_reporters(){let directory=tempfile::tempdir().expect("dir");let path=directory.path().join("herdr-managed.ts");std::fs::write(&path,b"HERDR_INTEGRATION_ID=managed").expect("fixture");assert!(!has_user_reporter(&[path.to_string_lossy().into_owned()]));std::fs::write(&path,b"user reporter").expect("fixture");assert!(has_user_reporter(&[path.to_string_lossy().into_owned()]));assert!(has_user_reporter(&[directory.path().join("herdr-missing.js").to_string_lossy().into_owned()]));}
#[test]
fn child_ownership(){let directory=tempfile::tempdir().expect("dir");let tasks=directory.path().join(".omo/senpi-task/tasks");std::fs::create_dir_all(&tasks).expect("mkdir");for (name,value) in [("root",serde_json::json!({"status":"running","root_session_id":"root"})),("parent",serde_json::json!({"status":"pending","parent_session_id":"root"})),("other",serde_json::json!({"status":"running","parent_session_id":"other"})),("done",serde_json::json!({"status":"done","root_session_id":"root"}))]{std::fs::write(tasks.join(format!("{name}.json")),value.to_string()).expect("fixture");}std::fs::write(tasks.join("partial.json"),"{").expect("fixture");assert_eq!(count_running_child_tasks(directory.path(),"root"),2);}

struct Session { id: String, file: Option<std::path::PathBuf>, name: Option<String> }
impl ToolSessionManager for Session {
    fn session_id(&self) -> &str { &self.id }
    fn session_file(&self) -> Option<&Path> { self.file.as_deref() }
}
impl SessionManager for Session {
    fn get_entries(&self) -> Vec<SessionEntry> { Vec::new() }
    fn get_branch(&self) -> Vec<SessionEntry> { Vec::new() }
    fn get_leaf_id(&self) -> Option<String> { None }
    fn get_session_name(&self) -> Option<String> { self.name.clone() }
}
struct Registry;
impl ModelRegistry for Registry {
    fn get_all(&self) -> Vec<Model> { Vec::new() }
    fn get_available(&self) -> Vec<Model> { Vec::new() }
    fn find(&self, _: &str, _: &str) -> Option<Model> { None }
    fn has_configured_auth(&self, _: &Model) -> bool { false }
    fn get_api_key_for_provider<'a>(&'a self, _: &'a str) -> ExtensionFuture<'a, Option<String>> { Box::pin(async { Ok(None) }) }
}
struct Ui;
impl ExtensionUi for Ui {
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
    fn custom(&self, _: ComponentFactory, _: CustomUiOptions) -> ExtensionFuture<'_, JsonValue> { Box::pin(async { Err(ExtensionFailure::new("No custom UI")) }) }
    fn theme(&self) -> Theme { Theme::default() }
}
fn context(session: Arc<dyn SessionManager>, idle: Arc<AtomicBool>, mode: ExtensionMode, loaded: Vec<String>) -> ExtensionContext {
    ExtensionContext {
        ui: Arc::new(Ui), mode, has_ui: mode == ExtensionMode::Tui, cwd: "/tmp".into(), agent_dir: "/tmp/agent".into(),
        session_manager: session, model_registry: Arc::new(Registry), model: None, thinking_level: None,
        service_tier: None, effective_service_tier: None, scoped_models: Vec::new(), goal_store_file: None,
        loaded_extension_paths: loaded, signal: None, steering_signal: None,
        is_idle_fn: Arc::new(move || idle.load(Ordering::SeqCst)),
        wait_for_idle_fn: Arc::new(|| Box::pin(async {})),
        is_project_trusted_fn: Arc::new(|| true), is_compacting_fn: Arc::new(|| false),
        get_system_prompt_fn: Arc::new(String::new), get_system_prompt_options_fn: Arc::new(BuildSystemPromptOptions::default),
        registered_mcp_servers: Vec::new(), update_tool_hook_status: None, idle_coordinator: None, logger: None, defer_macrotask: None, compaction_signal: Default::default(),
    }
}
async fn dispatch(api: &ExtensionApi, kind: EventKind, event: &mut ExtensionEvent, ctx: &ExtensionContext) -> EventResult {
    let handler = api.registered.handlers[&kind][0].clone();
    handler(event, ctx).await.expect("handler result")
}
fn deps(socket: String, debug: Arc<Mutex<Vec<String>>>) -> HerdrDependencies {
    HerdrDependencies {
        env: Arc::new(move |name| match name { "HERDR_ENV" => Some("1".into()), "HERDR_SOCKET_PATH" => Some(socket.clone()), "HERDR_PANE_ID" => Some("pane-test".into()), _ => None }),
        now: Arc::new(|| 1000),
        debug: Arc::new(move |message| debug.lock().expect("debug").push(message.into())),
    }
}
type Reply = Arc<dyn Fn(&Value) -> Value + Send + Sync>;
async fn serve(listener: UnixListener, mut stop: tokio::sync::oneshot::Receiver<()>, recorded: Arc<Mutex<Vec<Value>>>, reply: Reply) {
    loop {
        tokio::select! {
            _ = &mut stop => break,
            accepted = listener.accept() => {
                let Ok((mut socket, _)) = accepted else { break };
                let mut bytes = Vec::new();
                loop {
                    let mut chunk = [0u8; 1];
                    match socket.read(&mut chunk).await { Ok(0) => break, Ok(_) => {}, Err(_) => break }
                    if chunk[0] == b'\n' { break; }
                    bytes.push(chunk[0]);
                }
                if let Ok(request) = serde_json::from_slice::<Value>(&bytes) {
                    let mut framed = serde_json::to_vec(&reply(&request)).expect("reply");
                    framed.push(b'\n');
                    let _ = socket.write_all(&framed).await;
                    recorded.lock().expect("recorded").push(request);
                }
            }
        }
    }
}
fn ack(request: &Value) -> Value { json!({ "id": request["id"].clone(), "result": {} }) }
fn wrong_id(_: &Value) -> Value { json!({ "id": "other", "result": {} }) }
fn recorded_methods(recorded: &Arc<Mutex<Vec<Value>>>) -> Vec<String> { recorded.lock().expect("recorded").iter().map(|request| request["method"].as_str().unwrap_or_default().to_owned()).collect() }
fn recorded_states(recorded: &Arc<Mutex<Vec<Value>>>) -> Vec<String> { recorded.lock().expect("recorded").iter().filter(|request| request["method"] == "pane.report_agent").map(|request| request["params"]["state"].as_str().unwrap_or_default().to_owned()).collect() }

struct Fixture {
    directory: tempfile::TempDir,
    recorded: Arc<Mutex<Vec<Value>>>,
    session: Arc<dyn SessionManager>,
    idle: Arc<AtomicBool>,
    socket: String,
    stop: tokio::sync::oneshot::Sender<()>,
    server: tokio::task::JoinHandle<()>,
}
impl Fixture {
    async fn finish(self) {
        self.stop.send(()).expect("stop server");
        self.server.await.expect("server teardown");
        drop(self.directory);
    }
}
async fn fixture() -> Fixture {
    let directory = tempfile::tempdir().expect("dir");
    let path = directory.path().join("sock");
    let listener = UnixListener::bind(&path).expect("listener");
    let recorded = Arc::new(Mutex::new(Vec::new()));
    let (stop, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(serve(listener, stop_rx, recorded.clone(), Arc::new(ack)));
    let session: Arc<dyn SessionManager> = Arc::new(Session { id: "root-session".into(), file: Some("/sessions/root.jsonl".into()), name: Some("Reporter QA".into()) });
    let idle = Arc::new(AtomicBool::new(true));
    Fixture { directory, recorded, session, idle, socket: path.to_string_lossy().into_owned(), stop, server }
}

#[tokio::test]
async fn lifecycle_transcript_reports_live_bus_and_shutdown_releases_once() {
    let f = fixture().await;
    let mut api = ExtensionApi::new(LoadedExtension::new("herdr", "/tmp".into(), SourceInfo::default()), ExtensionSessionProfile::default(), EventBus::default(), ExtensionRuntime::default());
    HerdrExtension { deps: deps(f.socket.clone(), Arc::new(Mutex::new(Vec::new()))) }.register(&mut api);
    let ctx = context(f.session.clone(), f.idle.clone(), ExtensionMode::Tui, Vec::new());

    dispatch(&api, EventKind::SessionStart, &mut ExtensionEvent::SessionStart(SessionStartEvent { reason: SessionReason::Startup, initial_model_provenance: None, previous_session_file: None }), &ctx).await;
    assert_eq!(recorded_methods(&f.recorded), vec!["pane.report_metadata", "pane.report_agent_session", "pane.report_agent"]);
    assert_eq!(recorded_states(&f.recorded), vec!["idle"]);
    let first = f.recorded.lock().expect("recorded").clone();
    assert_eq!(first[0]["params"]["title"], "Reporter QA");
    assert_eq!(first[1]["params"]["agent_session_path"], "/sessions/root.jsonl");
    for request in &first { assert_eq!(request["params"]["source"], "custom:senpi"); assert_eq!(request["params"]["pane_id"], "pane-test"); }

    api.events.emit("herdr:blocked", &json!({ "active": true, "id": "q1", "label": "Auth — Which flow?" }));
    api.events.emit("herdr:blocked", &json!({ "active": true, "id": "q1", "label": "duplicate" }));
    assert_eq!(recorded_states(&f.recorded), vec!["idle"], "the queued report is not delivered until shutdown drains it");

    dispatch(&api, EventKind::SessionShutdown, &mut ExtensionEvent::SessionShutdown(SessionShutdownEvent { reason: SessionReason::Quit, target_session_file: None, signal: None }), &ctx).await;
    assert_eq!(recorded_states(&f.recorded), vec!["idle", "blocked"]);
    assert_eq!(f.recorded.lock().expect("recorded").last().expect("release")["method"], "pane.release_agent");
    assert_eq!(recorded_methods(&f.recorded).iter().filter(|method| method.as_str() == "pane.release_agent").count(), 1);
    assert!(f.recorded.lock().expect("recorded").iter().any(|request| request["params"]["message"] == "Auth — Which flow?"));

    dispatch(&api, EventKind::SessionShutdown, &mut ExtensionEvent::SessionShutdown(SessionShutdownEvent { reason: SessionReason::Quit, target_session_file: None, signal: None }), &ctx).await;
    api.events.emit("herdr:blocked", &json!({ "active": true, "id": "late", "label": "late" }));
    api.events.emit("terminal_monitor_state", &json!({ "activeCount": 4 }));
    f.idle.store(false, Ordering::SeqCst);
    dispatch(&api, EventKind::AgentStart, &mut ExtensionEvent::AgentStart, &ctx).await;
    assert_eq!(recorded_methods(&f.recorded).iter().filter(|method| method.as_str() == "pane.release_agent").count(), 1);
    assert_eq!(recorded_states(&f.recorded), vec!["idle", "blocked"]);

    f.finish().await;
}

#[tokio::test]
async fn concurrent_shutdown_disposes_once_and_keeps_live_bus_delivery() {
    let f = fixture().await;
    let mut api = ExtensionApi::new(LoadedExtension::new("herdr", "/tmp".into(), SourceInfo::default()), ExtensionSessionProfile::default(), EventBus::default(), ExtensionRuntime::default());
    HerdrExtension { deps: deps(f.socket.clone(), Arc::new(Mutex::new(Vec::new()))) }.register(&mut api);
    let ctx = context(f.session.clone(), f.idle.clone(), ExtensionMode::Tui, Vec::new());

    dispatch(&api, EventKind::SessionStart, &mut ExtensionEvent::SessionStart(SessionStartEvent { reason: SessionReason::Startup, initial_model_provenance: None, previous_session_file: None }), &ctx).await;
    api.events.emit("herdr:blocked", &json!({ "active": true, "id": "q1", "label": "Question" }));

    let handler = api.registered.handlers[&EventKind::SessionShutdown][0].clone();
    let mut shutdown_a = ExtensionEvent::SessionShutdown(SessionShutdownEvent { reason: SessionReason::Quit, target_session_file: None, signal: None });
    let mut shutdown_b = ExtensionEvent::SessionShutdown(SessionShutdownEvent { reason: SessionReason::Quit, target_session_file: None, signal: None });
    let (result_a, result_b) = tokio::join!(handler(&mut shutdown_a, &ctx), handler(&mut shutdown_b, &ctx));
    result_a.expect("first shutdown");
    result_b.expect("second shutdown");

    assert_eq!(recorded_states(&f.recorded), vec!["idle", "blocked"]);
    assert_eq!(recorded_methods(&f.recorded).iter().filter(|method| method.as_str() == "pane.release_agent").count(), 1);

    f.finish().await;
}

#[tokio::test]
async fn defers_to_user_reporter_and_ignores_non_tui_or_missing_env() {
    let f = fixture().await;
    let user = f.directory.path().join("herdr-user.ts");
    std::fs::write(&user, "user reporter").expect("fixture");
    let mut api = ExtensionApi::new(LoadedExtension::new("herdr", "/tmp".into(), SourceInfo::default()), ExtensionSessionProfile::default(), EventBus::default(), ExtensionRuntime::default());
    HerdrExtension { deps: deps(f.socket.clone(), Arc::new(Mutex::new(Vec::new()))) }.register(&mut api);

    let deferred = context(f.session.clone(), f.idle.clone(), ExtensionMode::Tui, vec![user.to_string_lossy().into_owned()]);
    dispatch(&api, EventKind::SessionStart, &mut ExtensionEvent::SessionStart(SessionStartEvent { reason: SessionReason::Startup, initial_model_provenance: None, previous_session_file: None }), &deferred).await;
    dispatch(&api, EventKind::SessionStart, &mut ExtensionEvent::SessionStart(SessionStartEvent { reason: SessionReason::Startup, initial_model_provenance: None, previous_session_file: None }), &deferred).await;
    assert!(f.recorded.lock().expect("recorded").is_empty());

    let mut non_tui = ExtensionApi::new(LoadedExtension::new("herdr", "/tmp".into(), SourceInfo::default()), ExtensionSessionProfile::default(), EventBus::default(), ExtensionRuntime::default());
    HerdrExtension { deps: deps(f.socket.clone(), Arc::new(Mutex::new(Vec::new()))) }.register(&mut non_tui);
    let print = context(f.session.clone(), f.idle.clone(), ExtensionMode::Print, Vec::new());
    dispatch(&non_tui, EventKind::SessionStart, &mut ExtensionEvent::SessionStart(SessionStartEvent { reason: SessionReason::Startup, initial_model_provenance: None, previous_session_file: None }), &print).await;
    assert!(f.recorded.lock().expect("recorded").is_empty());

    let mut missing = ExtensionApi::new(LoadedExtension::new("herdr", "/tmp".into(), SourceInfo::default()), ExtensionSessionProfile::default(), EventBus::default(), ExtensionRuntime::default());
    let absent = HerdrDependencies { env: Arc::new(|_| None), now: Arc::new(|| 0), debug: Arc::new(|_| {}) };
    HerdrExtension { deps: absent }.register(&mut missing);
    let ctx = context(f.session.clone(), f.idle.clone(), ExtensionMode::Tui, Vec::new());
    dispatch(&missing, EventKind::SessionStart, &mut ExtensionEvent::SessionStart(SessionStartEvent { reason: SessionReason::Startup, initial_model_provenance: None, previous_session_file: None }), &ctx).await;
    assert!(f.recorded.lock().expect("recorded").is_empty());

    f.finish().await;
}

#[tokio::test]
async fn logs_failed_delivery_and_retries_unchanged_state_on_the_next_signal() {
    let directory = tempfile::tempdir().expect("dir");
    let path = directory.path().join("sock");
    let listener = UnixListener::bind(&path).expect("listener");
    let recorded = Arc::new(Mutex::new(Vec::new()));
    let (stop, stop_rx) = tokio::sync::oneshot::channel::<()>();
    let failing = Arc::new(AtomicBool::new(true));
    let flag = failing.clone();
    let reply: Reply = Arc::new(move |request: &Value| if flag.load(Ordering::SeqCst) { wrong_id(request) } else { ack(request) });
    let server = tokio::spawn(serve(listener, stop_rx, recorded.clone(), reply));

    let debug = Arc::new(Mutex::new(Vec::new()));
    let mut api = ExtensionApi::new(LoadedExtension::new("herdr", "/tmp".into(), SourceInfo::default()), ExtensionSessionProfile::default(), EventBus::default(), ExtensionRuntime::default());
    HerdrExtension { deps: deps(path.to_string_lossy().into_owned(), debug.clone()) }.register(&mut api);
    let session: Arc<dyn SessionManager> = Arc::new(Session { id: "root-session".into(), file: None, name: None });
    let idle = Arc::new(AtomicBool::new(true));
    let ctx = context(session, idle, ExtensionMode::Tui, Vec::new());

    dispatch(&api, EventKind::SessionStart, &mut ExtensionEvent::SessionStart(SessionStartEvent { reason: SessionReason::Startup, initial_model_provenance: None, previous_session_file: None }), &ctx).await;
    assert_eq!(debug.lock().expect("debug").len(), 3);
    assert!(recorded_states(&recorded).is_empty());

    failing.store(false, Ordering::SeqCst);
    dispatch(&api, EventKind::AgentSettled, &mut ExtensionEvent::AgentSettled, &ctx).await;
    assert_eq!(recorded_states(&recorded), vec!["idle"]);
    assert_eq!(recorded.lock().expect("recorded").last().expect("report")["params"]["agent_session_id"], "root-session");

    stop.send(()).expect("stop server");
    server.await.expect("server teardown");
    drop(directory);
}
