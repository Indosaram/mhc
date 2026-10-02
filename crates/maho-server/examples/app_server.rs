use maho_server::app_server::{cli_args::{CliArgs, parse_cli_args}, index::run_app_server_mode, runtime::AppServerRuntime};
use futures_util::{SinkExt,StreamExt};
use serde_json::{Value,json};
use tokio_tungstenite::{client_async,tungstenite::{Message,client::IntoClientRequest}};

type QaClient = tokio_tungstenite::WebSocketStream<tokio::net::UnixStream>;
async fn qa_read(client: &mut QaClient) -> Result<Value,Box<dyn std::error::Error>> {
    let message = tokio::time::timeout(std::time::Duration::from_secs(15),client.next()).await?.ok_or("daemon client closed")??;
    Ok(serde_json::from_str(message.to_text()?)?)
}
async fn qa_send(client: &mut QaClient,message: Value) -> Result<(),Box<dyn std::error::Error>> {client.send(Message::Text(message.to_string().into())).await?;Ok(())}
async fn qa_attach(path: &std::path::Path,token: &str) -> Result<QaClient,Box<dyn std::error::Error>> {
    let mut request = "ws://localhost/".into_client_request()?;
    request.headers_mut().insert("authorization",format!("Bearer {token}").parse()?);
    let (mut client,_) = client_async(request,tokio::net::UnixStream::connect(path).await?).await?;
    qa_send(&mut client,json!({"id":1,"method":"initialize","params":{"clientInfo":{"name":"qa","version":"1"},"capabilities":{"experimentalApi":true}}})).await?;
    if qa_read(&mut client).await?["id"] != 1 {return Err("initialize response mismatch".into());}Ok(client)
}
async fn qa_native_session(socket: &std::path::Path,token_path: &std::path::Path) -> Result<Value,Box<dyn std::error::Error>> {
    let token = tokio::fs::read_to_string(token_path).await?;
    let token = maho_ai::utils::js::trim(&token);
    let mut client = qa_attach(socket,token).await?;
    qa_send(&mut client,json!({"id":2,"method":"thread/start","params":{}})).await?;
    let started = qa_read(&mut client).await?;let id = started["result"]["thread"]["id"].as_str().ok_or("missing thread id")?.to_owned();
    if qa_read(&mut client).await?["method"] != "thread/started" {return Err("missing thread started".into());}
    qa_send(&mut client,json!({"id":3,"method":"turn/start","params":{"threadId":id,"input":[{"type":"text","text":"managed faux daemon QA"}]}})).await?;
    if qa_read(&mut client).await?["id"] != 3 {return Err("missing turn acknowledgement".into());}
    let mut events = Vec::new();loop {let event = qa_read(&mut client).await?;let done = event["method"] == "turn/completed";events.push(event);if done {break;}}
    client.close(None).await?;drop(client);
    let mut client = qa_attach(socket,token).await?;
    qa_send(&mut client,json!({"id":4,"method":"thread/resume","params":{"threadId":id}})).await?;
    let resumed = qa_read(&mut client).await?;
    if resumed["result"]["thread"]["id"] != id || resumed["result"]["thread"]["turns"].as_array().is_none_or(Vec::is_empty) {return Err("reattach lost retained history".into());}
    if qa_read(&mut client).await?["method"] != "thread/status/changed" {return Err("reattach lifecycle missing".into());}
    qa_send(&mut client,json!({"id":5,"method":"thread/loaded/list","params":{}})).await?;
    if !qa_read(&mut client).await?["result"]["data"].as_array().is_some_and(|data|data.contains(&json!(id))) {return Err("reattached thread not loaded".into());}
    client.close(None).await?;Ok(json!({"threadId":id,"turnEvents":events.len(),"reattached":true,"retainedTurns":resumed["result"]["thread"]["turns"].as_array().map(Vec::len)}))
}
fn qa_factory() -> maho_server::app_server::thread_registry::SessionFactory {
    std::sync::Arc::new(|options|Box::pin(async move {
        use maho_ai::providers::faux::*;
        use maho_core::agent_session::{AgentSession,AgentSessionConfig};
        let cwd = options.cwd.unwrap_or_default();
        let provider = faux_provider(RegisterFauxProviderOptions {api:Some("faux".into()),models:Some(vec![FauxModelDefinition {id:"faux-1".into(),reasoning:Some(true),..Default::default()}]),tokens_per_second:Some(0.0),..Default::default()});
        let model = provider.get_model(Some("faux-1")).ok_or("missing faux model")?;
        provider.set_responses(vec![faux_assistant_message("retained managed transcript",FauxAssistantMessageOptions {timestamp:Some(0),..Default::default()}).into()]);
        let streams = faux_streams(provider.core.clone());
        let stream_fn: maho_agent::types::StreamFn = std::sync::Arc::new(move |model,context,options|streams.stream_simple(model,context,options.map(|options|options.simple)));
        let runtime = maho_core::model_runtime::ModelRuntime::create_sync(maho_core::model_runtime::CreateModelRuntimeOptions {models_path:Some(std::path::Path::new(&cwd).join("models.json")),auth_path:Some(std::path::Path::new(&cwd).join("auth.json")),providers:Some(vec![provider.provider.clone()]),..Default::default()});
        AgentSession::new(AgentSessionConfig {
            agent:maho_agent::Agent::new(maho_agent::AgentOptions {initial_state:Some(maho_agent::agent::PartialAgentState {model:Some(model),..Default::default()}),stream_fn:Some(stream_fn),..Default::default()}),
            session_manager:options.session_manager.ok_or("Missing session manager")?,settings_manager:maho_core::settings_manager::SettingsManager::from_storage(Box::new(maho_core::settings_manager::InMemorySettingsStorage::default()),false),
            cwd:cwd.clone(),agent_dir:Some(cwd),fallback_now:Some(std::sync::Arc::new(||0.0)),retry_random:Some(std::sync::Arc::new(||0.5)),scoped_models:Vec::new(),favorite_models:Vec::new(),flag_values:Default::default(),custom_tools:Vec::new(),model_runtime:Some(runtime),model_registry:None,uses_default_stream_function:Some(false),initial_active_tool_names:None,default_tool_names:None,eval_only_tool_names:None,allowed_tool_names:None,excluded_tool_names:None,base_tools_override:None,session_start_event:None,auto_title_sessions:Some(false),
        }).map_err(|error|error.to_string())
    }))
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.first().is_some_and(|arg|arg == "qa-daemon") {
        use maho_server::app_server::{daemon::{DaemonPaths,run_daemon_command},cli_args::DaemonVerb};
        let directory = tempfile::tempdir()?;
        let paths = DaemonPaths::new(directory.path());
        let socket = directory.path().join("app.sock");
        let listen = serde_json::json!({"kind":"unix","url":format!("unix://{}",socket.display()),"path":socket});
        let executable = std::env::current_exe()?;
        let started = run_daemon_command(&paths,DaemonVerb::Start,&listen,"1",&executable,&["qa-faux-server".into()]).await?;
        let status = run_daemon_command(&paths,DaemonVerb::Status,&listen,"1",&executable,&[]).await;
        let session = qa_native_session(&socket,&paths.token_file).await;
        let stopped = run_daemon_command(&paths,DaemonVerb::Stop,&listen,"1",&executable,&[]).await?;
        let status = status?;
        let session = session?;
        if started["status"] != "started" || status["status"] != "running" || stopped["status"] != "stopped" || socket.exists() || paths.pid_file.exists() || paths.settings_file.exists() {return Err("Daemon QA lifecycle or cleanup failed".into());}
        println!("{}",serde_json::json!({"started":started,"status":status,"session":session,"stopped":stopped,"cleanup":{"socketRemoved":true,"pidFileRemoved":true,"settingsRemoved":true}}));
        return Ok(());
    }
    let faux = args.first().is_some_and(|arg|arg == "qa-faux-server");
    let args = if faux {&args[1..]} else {&args};
    let args = args.strip_prefix(&["app-server".to_owned()]).unwrap_or(args);
    let CliArgs::Server {listen,ws_auth,..} = parse_cli_args(args) else {return Err("Expected app-server mode arguments".into());};
    let cwd = std::env::current_dir()?.display().to_string();
    let agent_dir = maho_core::config::get_agent_dir();
    let session_dir = if faux {Some(std::path::Path::new(&agent_dir).join("qa-sessions").display().to_string())} else {std::env::var("MAHO_SESSION_DIR").ok()};
    let runtime = AppServerRuntime::new(agent_dir,cwd,"1".into(),session_dir,faux.then(qa_factory)).await;
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    run_app_server_mode(&runtime,listen,ws_auth,async move {
        tokio::select! {_ = terminate.recv() => {}, result = tokio::signal::ctrl_c() => {if let Err(error) = result {eprintln!("app-server signal: {error}");}}}
    }).await.map_err(Into::into)
}
