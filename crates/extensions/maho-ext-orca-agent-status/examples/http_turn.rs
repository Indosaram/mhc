use maho_ext_api::*;
use std::{io::{BufRead, Read, Write}, path::Path, sync::{Arc, mpsc}, time::Duration};

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
    ExtensionContext { ui: Arc::new(Ui), mode: ExtensionMode::Print, has_ui: false, cwd: "/tmp".into(), agent_dir: "/tmp/agent".into(),
        session_manager: Arc::new(Session), model_registry: Arc::new(Registry), model: None, thinking_level: None,
        service_tier: None, effective_service_tier: None, scoped_models: Vec::new(), goal_store_file: None,
        loaded_extension_paths: Vec::new(), signal: None, steering_signal: None,
        is_idle_fn: Arc::new(|| true), wait_for_idle_fn: Arc::new(|| Box::pin(async {})), is_project_trusted_fn: Arc::new(|| true),
        is_compacting_fn: Arc::new(|| false), get_system_prompt_fn: Arc::new(String::new),
        get_system_prompt_options_fn: Arc::new(BuildSystemPromptOptions::default), registered_mcp_servers: Vec::new(), update_tool_hook_status: None }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let port: u16 = std::env::var("ORCA_AGENT_HOOK_PORT")?.parse()?;
    let listener = std::net::TcpListener::bind(("127.0.0.1", port))?;
    let (sender, receiver) = mpsc::channel();
    let server = std::thread::spawn(move || -> std::io::Result<()> {
        for _ in 0..4 {
            let (mut stream, _) = listener.accept()?;
            stream.set_read_timeout(Some(Duration::from_secs(3)))?;
            let mut reader = std::io::BufReader::new(stream.try_clone()?);
            let mut line = String::new();
            reader.read_line(&mut line)?;
            if !line.starts_with("POST /hook/pi ") { return Err(std::io::Error::other("unexpected HTTP route")); }
            let mut length = 0;
            loop {
                line.clear(); reader.read_line(&mut line)?;
                if line == "\r\n" { break; }
                if let Some(value) = line.to_lowercase().strip_prefix("content-length:") { length = value.trim().parse().map_err(std::io::Error::other)?; }
            }
            let mut body = vec![0; length]; reader.read_exact(&mut body)?;
            stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")?;
            sender.send(body).map_err(std::io::Error::other)?;
        }
        Ok(())
    });
    let mut runner = maho_ext_host::ExtensionRunner::from_static(vec![Box::new(maho_ext_orca_agent_status::OrcaAgentStatus)], context());
    let events = [
        ExtensionEvent::SessionStart(SessionStartEvent { reason: SessionReason::Startup, initial_model_provenance: None, previous_session_file: None }),
        ExtensionEvent::BeforeAgentStart(BeforeAgentStartEvent { prompt: "faux prompt".into(), images: None, system_prompt: String::new(), system_prompt_options: Default::default() }),
        ExtensionEvent::AgentStart,
        ExtensionEvent::AgentSettled,
    ];
    for event in events {
        runner.emit(event).await?;
        let body = receiver.recv_timeout(Duration::from_secs(3))?;
        println!("{}", String::from_utf8(body)?);
    }
    if !runner.errors.is_empty() { return Err("extension handler failed".into()); }
    drop(runner);
    server.join().map_err(|_| "capture server panicked")??;
    Ok(())
}
