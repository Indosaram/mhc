use super::{args::Args, startup};
use maho_core::{agent_session::PromptOptions, project_trust::AppMode, session_manager::SessionManager};
use std::{io::IsTerminal, sync::Arc};

pub async fn run(mut parsed: Args) -> Result<(), String> {
    super::setup::register_builtin_apis();
    let stdin_tty = std::io::stdin().is_terminal();
    let mode = startup::resolve_app_mode(&parsed, stdin_tty, std::io::stdout().is_terminal());
    if mode == AppMode::AppServer { return Err("App-server execution blocked by unmerged todo 37 (maho-server)".into()); }
    let cwd = std::env::current_dir().map_err(|error| error.to_string())?;
    let cwd_text = cwd.to_string_lossy();
    let agent_dir = maho_core::config::get_agent_dir();
    let mut indicator = super::startup_loading_indicator::StartupLoadingIndicator::new(
        super::startup_loading_indicator::StartupLoadingIndicatorOptions::new(|text| {
            use std::io::Write;
            if let Err(error) = std::io::stderr().lock().write_all(text.as_bytes()) { eprintln!("{error}"); }
        }, mode == AppMode::Interactive && std::io::stdout().is_terminal())
    );
    indicator.start();
    indicator.set_phase(Some("loading settings".into()));
    let migrations = crate::migrations::run_migrations(&cwd, std::path::Path::new(&maho_core::config::home_dir()), std::path::Path::new(&agent_dir)).map_err(|error| error.to_string())?;
    let settings = maho_core::settings_manager::SettingsManager::create(&cwd_text, &agent_dir, &maho_core::config::home_dir(), false);
    let models = maho_core::model_runtime::ModelRuntime::create_sync(maho_core::model_runtime::CreateModelRuntimeOptions {
        models_path: Some(std::path::Path::new(&agent_dir).join("models.json")),
        auth_path: Some(std::path::Path::new(&agent_dir).join("auth.json")), ..Default::default()
    });
    let scope = maho_core::model_resolver::resolve_model_scope_from_models(parsed.models.as_deref().unwrap_or_default(), &models.get_models(None));
    let options = startup::build_session_options(&parsed, &scope.scoped_models, parsed.session.is_some() || parsed.continue_session, &models, &settings);
    for diagnostic in &options.diagnostics { eprintln!("{}: {}", if diagnostic.error { "Error" } else { "Warning" }, diagnostic.message); }
    if options.diagnostics.iter().any(|diagnostic| diagnostic.error) { return Err("Invalid model selection".into()); }
    let identity = Some(maho_core::session_manager::NewSessionOptions { id: parsed.session_id.clone(), ..Default::default() });
    let manager = if parsed.no_session { SessionManager::in_memory(&cwd_text, identity, None) }
        else if let Some(path) = &parsed.session { SessionManager::open(path, parsed.session_dir.as_deref(), None, identity) }
        else if parsed.continue_session { SessionManager::continue_recent(&cwd_text, parsed.session_dir.as_deref()) }
        else { SessionManager::create(&cwd_text, parsed.session_dir.as_deref(), identity) };
    let created = maho_core::sdk::create_agent_session(maho_core::sdk::CreateAgentSessionOptions {
        cwd: Some(cwd_text.into_owned()), agent_dir: Some(agent_dir), model_runtime: Some(models),
        settings_manager: Some(settings), session_manager: Some(manager), model: options.options.model,
        tools: options.options.tools, exclude_tools: options.options.exclude_tools, no_tools: options.options.no_tools,
        thinking_selection: options.options.thinking_selection, ..Default::default()
    }).await?;
    let session = Arc::new(created.session);
    if let Some(level) = options.options.thinking_level { session.set_session_thinking_level(level); }
    if let Some(name) = &parsed.name { session.set_session_name(name); }
    indicator.stop();
    if mode == AppMode::Interactive { for warning in migrations.deprecation_warnings { eprintln!("{warning}"); } }
    let result = async { match mode {
        AppMode::Rpc => maho_rpc::rpc_mode::run_command_stream(&session, tokio::io::stdin(), tokio::io::stdout()).await.map_err(|error| error.to_string()),
        AppMode::Print | AppMode::Json => {
            use tokio::io::AsyncReadExt;
            let stdin = if stdin_tty { None } else { let mut text = String::new(); tokio::io::stdin().read_to_string(&mut text).await.map_err(|error| error.to_string())?; Some(text) };
            let initial = startup::prepare_initial_message(&mut parsed, &cwd, true, stdin.as_deref()).await?;
            run_print(&session, mode, initial, &parsed.messages).await
        }
        AppMode::Interactive => {
            let initial = startup::prepare_initial_message(&mut parsed, &cwd, true, None).await?;
            super::interactive_entry::run(session.clone(), &parsed, initial).await
        },
        AppMode::AppServer => unreachable!("server mode rejected before session creation"),
    } }.await;
    session.dispose().await;
    result
}

async fn run_print(session: &maho_core::agent_session::AgentSession, mode: AppMode, initial: super::initial_message::InitialMessageResult, messages: &[String]) -> Result<(), String> {
    use std::io::Write;
    let subscription = session.subscribe(Arc::new(move |event| {
        if let Some(diagnostic) = maho_rpc::print_mode::fallback_diagnostic(event) { eprint!("{diagnostic}"); }
    }));
    let json_subscription = if mode == AppMode::Json {
        Some(session.agent().subscribe(Arc::new(move |event, _signal| {
            Box::pin(async move {
                let result = (|| -> Result<(), String> {
                    let value = serde_json::to_value(event).map_err(|error| error.to_string())?;
                    let value = maho_rpc::json_event::to_json_event(&value).map_err(|error| error.to_string())?;
                    let line = maho_rpc::jsonl::serialize_json_line(value.as_ref()).map_err(|error| error.to_string())?;
                    std::io::stdout().lock().write_all(line.as_bytes()).map_err(|error| error.to_string())
                })();
                if let Err(error) = result { eprintln!("{error}"); }
            })
        })))
    } else { None };
    if let Some(text) = initial.initial_message { session.prompt(&text, PromptOptions { images: initial.initial_images, ..Default::default() }).await?; }
    for message in messages { session.prompt(message, PromptOptions::default()).await?; }
    session.wait_for_idle().await;
    drop(subscription);
    if let Some(subscription) = json_subscription { session.agent().unsubscribe(&subscription); }
    if mode == AppMode::Print {
        let messages = session.messages();
        let assistant = messages.iter().rev().find_map(|message| match message { maho_agent::types::AgentMessage::Llm(maho_ai::types::Message::Assistant(message)) => Some(message), _ => None });
        let output = maho_rpc::print_mode::format_print_result(assistant.map(|message| &**message)).map_err(|error| error.to_string())?;
        std::io::stdout().lock().write_all(output.stdout.as_bytes()).map_err(|error| error.to_string())?;
        if output.exit_code != 0 { return Err(output.stderr.trim_end().to_owned()); }
    }
    Ok(())
}
