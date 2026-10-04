use maho_ext_api::*;
use std::{io::BufRead, path::Path, sync::{Arc, mpsc}, time::Duration};

struct Session;
impl ToolSessionManager for Session {
    fn session_id(&self) -> &str { "faux-session" }
    fn session_file(&self) -> Option<&Path> { None }
}
impl SessionManager for Session {
    fn get_entries(&self) -> Vec<SessionEntry> { Vec::new() }
    fn get_branch(&self) -> Vec<SessionEntry> { Vec::new() }
    fn get_leaf_id(&self) -> Option<String> { None }
    fn get_session_name(&self) -> Option<String> { None }
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
    fn custom(&self, _: ComponentFactory, _: CustomUiOptions) -> ExtensionFuture<'_, JsonValue> { Box::pin(async { Err("fixture has no UI".into()) }) }
    fn theme(&self) -> Theme { Theme::default() }
}
fn context() -> ExtensionContext {
    ExtensionContext { ui: Arc::new(Ui), mode: ExtensionMode::Tui, has_ui: false, cwd: "/tmp".into(), agent_dir: "/tmp/agent".into(),
        session_manager: Arc::new(Session), model_registry: Arc::new(Registry), model: None, thinking_level: None,
        service_tier: None, effective_service_tier: None, scoped_models: Vec::new(), goal_store_file: None,
        loaded_extension_paths: Vec::new(), signal: None, steering_signal: None,
        is_idle_fn: Arc::new(|| true), wait_for_idle_fn: Arc::new(|| Box::pin(async {})), is_project_trusted_fn: Arc::new(|| true),
        is_compacting_fn: Arc::new(|| false), get_system_prompt_fn: Arc::new(String::new),
        get_system_prompt_options_fn: Arc::new(BuildSystemPromptOptions::default), registered_mcp_servers: Vec::new(), update_tool_hook_status: None, idle_coordinator: None, logger: None, defer_macrotask: None, compaction_signal: Default::default() }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let port: u16 = std::env::var("FERRYX_AGENT_STATE_PORT")?.parse()?;
    let listener = std::net::TcpListener::bind(("127.0.0.1", port))?;
    let (sender, receiver) = mpsc::channel();
    let peer = std::thread::spawn(move || -> std::io::Result<()> {
        for _ in 0..4 {
            let (stream, _) = listener.accept()?;
            stream.set_read_timeout(Some(Duration::from_secs(3)))?;
            let mut body = String::new();
            std::io::BufReader::new(stream).read_line(&mut body)?;
            sender.send(body).map_err(std::io::Error::other)?;
        }
        Ok(())
    });
    let mut runner = maho_ext_host::ExtensionRunner::from_static(vec![Box::new(maho_ext_ferryx_agent_state::FerryxAgentState)], context());
    runner.emit(ExtensionEvent::SessionStart(SessionStartEvent { reason: SessionReason::Startup, initial_model_provenance: None, previous_session_file: None })).await?;
    let body: JsonValue = serde_json::from_str(&receiver.recv_timeout(Duration::from_secs(3))?)?;
    assert_eq!(body["state"], "idle");
    assert_eq!(body["providerSession"]["id"], "faux-session");
    println!("{body}");
    runner.emit(ExtensionEvent::AgentStart).await?;
    let body: JsonValue = serde_json::from_str(&receiver.recv_timeout(Duration::from_secs(3))?)?;
    assert_eq!(body["state"], "working");
    println!("{body}");
    runner.events.emit("herdr:blocked", &serde_json::json!({"active":true,"id":"fixture","label":"question"}));
    let body: JsonValue = serde_json::from_str(&receiver.recv_timeout(Duration::from_secs(3))?)?;
    assert_eq!(body["state"], "blocked");
    assert_eq!(body["detail"], "question");
    println!("{body}");
    runner.events.emit("herdr:blocked", &serde_json::json!({"active":false,"id":"fixture"}));
    let body: JsonValue = serde_json::from_str(&receiver.recv_timeout(Duration::from_secs(3))?)?;
    assert_eq!(body["state"], "working");
    println!("{body}");
    runner.emit(ExtensionEvent::SessionShutdown(SessionShutdownEvent { reason: SessionReason::Startup, target_session_file: None, signal: None })).await?;
    drop(runner);
    peer.join().map_err(|_| "peer panicked")??;
    Ok(())
}
