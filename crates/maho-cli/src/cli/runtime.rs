use super::{args::Args, startup};
use maho_core::{agent_session::PromptOptions, project_trust::AppMode, session_manager::SessionManager};
use std::{io::IsTerminal, sync::Arc};

pub async fn run(mut parsed: Args, argv: &[String]) -> Result<(), String> {
    super::setup::register_builtin_apis();
    let stdin_tty = std::io::stdin().is_terminal();
    let mode = startup::resolve_app_mode(&parsed, stdin_tty, std::io::stdout().is_terminal());
    let cwd = std::env::current_dir().map_err(|error| error.to_string())?;
    let cwd_text = cwd.to_string_lossy();
    let agent_dir = maho_core::config::get_agent_dir();
    if mode == AppMode::AppServer {
        let config = super::host_runtime::CliRuntimeConfiguration::from_parsed(&parsed, &cwd_text, &agent_dir, AppMode::AppServer);
        let executable = std::env::current_exe().map_err(|error| error.to_string())?;
        let code = super::app_server::run_app_server_with_signals(argv, config, env!("CARGO_PKG_VERSION"), parsed.session_dir.clone(), &executable, &[]).await?;
        if code != 0 { return Err(format!("app-server exited with code {code}")); }
        return Ok(());
    }
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
    let identity = Some(maho_core::session_manager::NewSessionOptions { id: parsed.session_id.clone(), ..Default::default() });
    let manager = if parsed.no_session { SessionManager::in_memory(&cwd_text, identity, None) }
        else if let Some(path) = &parsed.session { SessionManager::open(path, parsed.session_dir.as_deref(), None, identity) }
        else if parsed.continue_session { SessionManager::continue_recent(&cwd_text, parsed.session_dir.as_deref()) }
        else { SessionManager::create(&cwd_text, parsed.session_dir.as_deref(), identity) };
    let (widget_sender, widget_requests) = tokio::sync::mpsc::unbounded_channel();
    let task_parent = Arc::new(std::sync::OnceLock::new());
    let mut base_factories = super::default_extensions::assembled_factories(widget_sender, task_parent.clone());
    let environment = super::oauth_providers::environment_from_process();
    let store = super::credentials::AuthStorageCredentialStore::create(
        &std::path::Path::new(&agent_dir).join("auth.json").to_string_lossy(),
    ).into_shared();
    let oauth = super::oauth_providers::oauth_extension_factories(
        super::oauth_providers::OAuthExtensionRequest {
            cwd: cwd.clone(), agent_dir: agent_dir.clone().into(), environment: environment.clone(),
            settings: super::oauth_providers::anthropic_subscription_settings(&settings, &environment),
            store: Arc::clone(&store),
        },
        super::oauth_providers::CursorAssembly {
            cwd: cwd.clone(), agent_dir: agent_dir.clone().into(), home: maho_core::config::home_dir().into(),
            environment, store,
            storage: Arc::new(maho_core::settings_manager::FileSettingsStorage::new(
                &cwd_text, &agent_dir, &maho_core::config::home_dir(),
            )),
        },
    );
    base_factories.extend(super::default_extensions::async_factories(oauth.factories));
    if parsed.no_extensions { base_factories.retain(|factory| factory.source_info.source != "user"); }
    let extensions = maho_ext_host::loader::load_extensions_async(base_factories.clone(), &cwd, Default::default()).await;
    let extension_snapshot = extensions.extensions.clone();
    for error in &extensions.errors { eprintln!("{}: {}", error.extension_path, error.error); }
    let providers = Arc::new(maho_core::agent_session_runtime::ExtensionModelRuntimeActions(std::sync::Mutex::new(models)));
    extensions.runtime.bind_providers(providers.clone()).map_err(|error| error.message)?;
    let models = providers.0.lock().map_err(|error| error.to_string())?.clone();
    let scope = maho_core::model_resolver::resolve_model_scope_from_models(parsed.models.as_deref().unwrap_or_default(), &models.get_models(None));
    let options = startup::build_session_options(&parsed, &scope.scoped_models, parsed.session.is_some() || parsed.continue_session, &models, &settings);
    for diagnostic in &options.diagnostics { eprintln!("{}: {}", if diagnostic.error { "Error" } else { "Warning" }, diagnostic.message); }
    if options.diagnostics.iter().any(|diagnostic| diagnostic.error) { return Err("Invalid model selection".into()); }
    let host_factory = maho_core::sdk::HostRuntimeFactory {
        model_registry: maho_core::model_registry::ModelRegistry::new(models.clone()),
        model_runtime: models, extension_factories: base_factories,
    };
    let created = host_factory.create(maho_core::sdk::CreateAgentSessionOptions {
        cwd: Some(cwd_text.to_string()), agent_dir: Some(agent_dir.clone()), loaded_extensions: Some(extensions),
        settings_manager: Some(settings), session_manager: Some(manager), model: options.options.model,
        tools: options.options.tools, exclude_tools: options.options.exclude_tools, no_tools: options.options.no_tools,
        thinking_selection: options.options.thinking_selection,
        system_prompt: parsed.system_prompt.clone(), append_system_prompt: parsed.append_system_prompt.clone(),
        scoped_models: startup::session_model_entries(options.options.scoped_models)?, defer_extension_start: true, ..Default::default()
    }).await?;
    let session = Arc::new(created.session);
    task_parent.set(session.weak_accessor()).map_err(|_| "Task parent already bound".to_owned())?;
    session.set_context_files_enabled(!parsed.no_context_files);
    let runtime_config = super::host_runtime::CliRuntimeConfiguration::from_parsed(&parsed, &cwd_text, &agent_dir, mode);
    let hook_sources = session.with_settings_manager(|settings| super::hook_sources::build_loaded_hook_sources(&runtime_config, settings));
    session.set_hook_sources(Some(hook_sources));
    if parsed.no_skills || parsed.no_prompt_templates || !parsed.skills.is_empty() || !parsed.prompt_templates.is_empty() {
        let templates = maho_core::prompt_templates::load_prompt_templates(&maho_core::prompt_templates::LoadPromptTemplatesOptions {
            cwd: cwd_text.to_string(), agent_dir: agent_dir.clone(), prompt_paths: parsed.prompt_templates.clone(),
            include_defaults: !parsed.no_prompt_templates,
        });
        let skills = maho_core::skills::load_skills(&maho_core::skills::LoadSkillsOptions {
            cwd: cwd_text.to_string(), agent_dir: agent_dir.clone(), skill_paths: parsed.skills.clone(), include_defaults: !parsed.no_skills,
        });
        for diagnostic in &skills.diagnostics { eprintln!("{}", diagnostic.message); }
        session.set_prompt_resources(templates, skills.skills);
        session.rebuild_system_prompt();
    }
    if let Some(level) = options.options.thinking_level { session.set_session_thinking_level(level); }
    if let Some(name) = &parsed.name { session.set_session_name(name); }
    indicator.stop();
    if mode == AppMode::Interactive { crate::migrations::show_deprecation_warnings(&migrations.deprecation_warnings).map_err(|error| error.to_string())?; }
    let result = async { match mode {
        AppMode::Rpc => {
            session.bind_extensions(maho_core::agent_session::ExtensionBindings {
                mode: Some(maho_ext_api::ExtensionMode::Rpc), ..Default::default()
            }).await;
            maho_rpc::rpc_mode::run_command_stream(&session, tokio::io::stdin(), tokio::io::stdout()).await.map_err(|error| error.to_string())
        },
        AppMode::Print | AppMode::Json => {
            session.bind_extensions(maho_core::agent_session::ExtensionBindings {
                mode: Some(if mode == AppMode::Json { maho_ext_api::ExtensionMode::Json } else { maho_ext_api::ExtensionMode::Print }),
                ..Default::default()
            }).await;
            use tokio::io::AsyncReadExt;
            let stdin = if stdin_tty { None } else { let mut text = String::new(); tokio::io::stdin().read_to_string(&mut text).await.map_err(|error| error.to_string())?; Some(text) };
            let initial = startup::prepare_initial_message(&mut parsed, &cwd, true, stdin.as_deref()).await?;
            run_print(&session, mode, initial, &parsed.messages).await
        }
        AppMode::Interactive => {
            let initial = startup::prepare_initial_message(&mut parsed, &cwd, true, None).await?;
            super::interactive_entry::run(session.clone(), &parsed, initial, extension_snapshot, widget_requests).await
        },
        AppMode::AppServer => unreachable!("server mode rejected before session creation"),
    } }.await;
    session.emit_session_shutdown(maho_ext_api::SessionReason::Quit).await;
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
    let result = async {
        if let Some(text) = initial.initial_message { session.prompt(&text, PromptOptions { images: initial.initial_images, ..Default::default() }).await?; }
        for message in messages { session.prompt(message, PromptOptions::default()).await?; }
        session.wait_for_idle().await;
        Ok::<(), String>(())
    }.await;
    drop(subscription);
    if let Some(subscription) = json_subscription { session.agent().unsubscribe(&subscription); }
    result?;
    if mode == AppMode::Print {
        let messages = session.messages();
        let assistant = messages.iter().rev().find_map(|message| match message { maho_agent::types::AgentMessage::Llm(maho_ai::types::Message::Assistant(message)) => Some(message), _ => None });
        let output = maho_rpc::print_mode::format_print_result(assistant.map(|message| &**message)).map_err(|error| error.to_string())?;
        std::io::stdout().lock().write_all(output.stdout.as_bytes()).map_err(|error| error.to_string())?;
        if output.exit_code != 0 { return Err(output.stderr.trim_end().to_owned()); }
    }
    Ok(())
}
