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
    if mode == AppMode::Rpc && parsed.multi_session {
        // senpi main.ts:1112-1154: a multi-session RPC host serves every session in this process,
        // built from the CLI configuration, instead of the single-session command stream. It never
        // returns until its watchdog or empty-exit policy ends it.
        let config = super::host_runtime::CliRuntimeConfiguration::from_parsed(&parsed, &cwd_text, &agent_dir, AppMode::Rpc);
        let core = super::host_runtime::create_host_core(config, &cwd_text, parsed.listen.as_deref());
        return super::host_runtime::serve_multi_session_host(core, parsed.listen.clone()).await.map_err(|error| error.to_string());
    }
    if mode != AppMode::Interactive { super::stdout_guard::take_over_stdout(); }
    struct InteractiveStderr;
    impl Drop for InteractiveStderr {
        fn drop(&mut self) { maho_interactive::interactive_stderr_guard::restore_interactive_stderr(); }
    }
    let _interactive_stderr = if mode == AppMode::Interactive {
        maho_interactive::interactive_stderr_guard::take_over_interactive_stderr();
        Some(InteractiveStderr)
    } else { None };
    let stdout_is_tty = std::io::stdout().is_terminal();
    let mut indicator = super::startup_loading_indicator::StartupLoadingIndicator::new(
        super::startup_loading_indicator::StartupLoadingIndicatorOptions::new(|text| {
            use std::io::Write;
            if let Err(error) = std::io::stdout().lock().write_all(text.as_bytes()) { maho_core::output_guard::maho_write_stderr(&format!("{error}\n")); }
        }, stdout_is_tty)
    );
    if super::startup_loading_indicator::should_show_startup_loading_indicator(mode, stdout_is_tty, parsed.help) {
        indicator.start();
        indicator.set_phase(Some("loading settings".into()));
    }
    let facts_bootstrap = std::env::var_os("MAHO_FACTS_READ_ONLY_BOOTSTRAP").is_some()
        && maho_core::lockfile_policy::facts_lock_broker_enabled();
    if facts_bootstrap && !parsed.no_session { return Err("facts read-only bootstrap requires --no-session".into()); }
    let migrations = if facts_bootstrap { crate::migrations::MigrationResult { migrated_auth_providers: vec![], deprecation_warnings: vec![] } }
    else { indicator.during_surface_write(|| crate::migrations::run_migrations_with_lock(&cwd, std::path::Path::new(&maho_core::config::home_dir()), std::path::Path::new(&agent_dir),true)).map_err(|error| error.to_string())? };
    // senpi `createCliRuntimeFactory`: resolve project trust for the launch cwd (override, saved
    // `trust.json`, `defaultProjectTrust`, then the interactive startup prompt) before project
    // settings and resources load.
    let trust_config = super::host_runtime::CliRuntimeConfiguration::from_parsed(&parsed, &cwd_text, &agent_dir, mode);
    let trust_mode = trust_config.trust_prompt_mode();
    let startup_ui = {
        let bootstrap = maho_core::settings_manager::SettingsManager::create(&cwd_text, &agent_dir, &maho_core::config::home_dir(), false);
        (bootstrap.get_string("theme"), bootstrap.get_bool("showHardwareCursor").unwrap_or(false), bootstrap.get_bool("clearOnShrink").unwrap_or(false))
    };
    let selector: Box<super::project_trust::Selector> = Box::new(move |title: &str, options: &[String]| {
        match super::startup_ui::show_startup_selector(startup_ui.0.as_deref(), startup_ui.1, startup_ui.2, title, options) {
            Ok(selected) => selected,
            Err(error) => { maho_core::output_guard::maho_write_stderr(&format!("{error}\n")); None }
        }
    });
    let project_trusted = indicator.during_prompt(super::host_runtime::resolve_runtime_project_trust(
        &trust_config, &cwd_text, trust_mode, trust_mode == AppMode::Interactive, selector,
    )).await;
    let mut settings = maho_core::settings_manager::SettingsManager::create(&cwd_text, &agent_dir, &maho_core::config::home_dir(), project_trusted);
    let models = maho_core::model_runtime::ModelRuntime::create_sync(maho_core::model_runtime::CreateModelRuntimeOptions {
        models_path: Some(std::path::Path::new(&agent_dir).join("models.json")),
        auth_path: Some(std::path::Path::new(&agent_dir).join("auth.json")), ..Default::default()
    });
    let identity = Some(maho_core::session_manager::NewSessionOptions { id: parsed.session_id.clone(), ..Default::default() });
    let manager = if parsed.no_session { SessionManager::in_memory(&cwd_text, identity, None) }
        else if let Some(source) = &parsed.fork { fork_session(source, &cwd_text, parsed.session_dir.as_deref(), parsed.session_id.as_deref())? }
        else if let Some(path) = &parsed.session { SessionManager::open(path, parsed.session_dir.as_deref(), None, identity) }
        else if parsed.continue_session { SessionManager::continue_recent(&cwd_text, parsed.session_dir.as_deref()) }
        else { SessionManager::create(&cwd_text, parsed.session_dir.as_deref(), identity) };
    let (widget_sender, widget_requests) = tokio::sync::mpsc::unbounded_channel();
    let task_parent = Arc::new(std::sync::OnceLock::new());
    let mut base_factories = indicator.during_surface_write(|| super::default_extensions::assembled_factories(widget_sender, task_parent.clone()));
    if !parsed.no_extensions {
        let discovery = super::user_extensions::resolve_and_discover(&mut settings, &cwd_text, &agent_dir, &parsed.extensions).await.map_err(|error| error.to_string())?;
        if !discovery.errors.is_empty() {
            return Err(discovery.errors.into_iter().map(|error| format!("{}: {}", error.path, error.message)).collect::<Vec<_>>().join("\n"));
        }
        base_factories.extend(discovery.factories);
    }
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
    let session_profile = super::shared_host::session_profile(mode, &settings, None, None);
    let extensions = indicator.during_prompt(maho_ext_host::loader::load_extensions_async(base_factories.clone(), &cwd, session_profile.clone())).await;
    let extension_snapshot = extensions.extensions.clone();
    indicator.during_surface_write(|| { for error in &extensions.errors { maho_core::output_guard::maho_write_stderr(&format!("{}: {}\n", error.extension_path, error.error)); } });
    let providers = Arc::new(maho_core::agent_session_runtime::ExtensionModelRuntimeActions(std::sync::Mutex::new(models)));
    extensions.runtime.bind_providers(providers.clone()).map_err(|error| error.message)?;
    for (name, value) in &parsed.unknown_flags {
        if extensions.extensions.iter().any(|extension| extension.flags.iter().any(|flag| flag.name == *name)) {
            let flag_value = match value {
                super::args::FlagValue::Boolean(value) => maho_ext_api::FlagValue::Boolean(*value),
                super::args::FlagValue::String(value) => maho_ext_api::FlagValue::String(value.clone()),
            };
            extensions.runtime.set_flag(name, flag_value);
        }
    }
    let models = providers.0.lock().map_err(|error| error.to_string())?.clone();
    let scope = maho_core::model_resolver::resolve_model_scope_from_models(parsed.models.as_deref().unwrap_or_default(), &models.get_models(None));
    let options = startup::build_session_options(&parsed, &scope.scoped_models, manager.has_context_messages(), &models, &settings);
    indicator.during_surface_write(|| { for diagnostic in &options.diagnostics { maho_core::output_guard::maho_write_stderr(&format!("{}: {}\n", if diagnostic.error { "Error" } else { "Warning" }, diagnostic.message)); } });
    if options.diagnostics.iter().any(|diagnostic| diagnostic.error) { return Err("Invalid model selection".into()); }
    let host_factory = maho_core::sdk::HostRuntimeFactory {
        model_registry: maho_core::model_registry::ModelRegistry::new(models.clone()),
        model_runtime: models, extension_factories: base_factories,
    };
    let created = host_factory.create(maho_core::sdk::CreateAgentSessionOptions {
        cwd: Some(cwd_text.to_string()), agent_dir: Some(agent_dir.clone()), loaded_extensions: Some(extensions),
        settings_manager: Some(settings), session_manager: Some(manager), model: options.options.model,
        initial_model_provenance: options.options.initial_model_provenance.map(str::to_owned),
        tools: options.options.tools, exclude_tools: options.options.exclude_tools, no_tools: options.options.no_tools,
        thinking_selection: options.options.thinking_selection,
        system_prompt: parsed.system_prompt.clone(), append_system_prompt: parsed.append_system_prompt.clone(),
        scoped_models: startup::session_model_entries(options.options.scoped_models)?, defer_extension_start: true,
        session_profile, ..Default::default()
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
        indicator.during_surface_write(|| { for diagnostic in &skills.diagnostics { maho_core::output_guard::maho_write_stderr(&format!("{}\n", diagnostic.message)); } });
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

pub fn fork_session(source_path: &str, cwd: &str, session_dir: Option<&str>, session_id: Option<&str>) -> Result<SessionManager, String> {
    let local_dir = session_dir.map(str::to_owned).unwrap_or_else(|| maho_core::session_manager::get_default_session_dir(cwd));
    let mut local = maho_core::session_discovery::list_sessions_from_dir(&local_dir, None, 0, None);
    local.sort_by_key(|session| std::cmp::Reverse(session.modified));
    let custom_sessions = local.clone();
    if let Some(id) = session_id
        && local.iter().any(|session| session.id == id)
    { return Err(format!("Session already exists with id '{id}'")); }
    if session_dir.is_some() && local_dir != maho_core::session_manager::get_default_session_dir(cwd) {
        let resolved_cwd = maho_core::paths::resolve_path(cwd, cwd, &maho_core::paths::PathInputOptions::default());
        local.retain(|session| maho_core::paths::resolve_path(&session.cwd, cwd, &maho_core::paths::PathInputOptions::default()) == resolved_cwd);
    }
    let resolved;
    let source_path = if source_path.contains('/') || source_path.contains('\\') || source_path.ends_with(".jsonl") {
        resolved = maho_core::paths::resolve_path(source_path, cwd, &maho_core::paths::PathInputOptions::expanding_tilde(maho_core::config::home_dir()));
        resolved.as_str()
    } else {
        let mut all = custom_sessions;
        if session_dir.is_none()
            && let Ok(directories) = std::fs::read_dir(maho_core::config::get_sessions_dir())
        {
            for directory in directories.filter_map(Result::ok).filter(|entry| entry.path().is_dir()) {
                all.extend(maho_core::session_discovery::list_sessions_from_dir(&directory.path().to_string_lossy(), None, 0, None));
            }
        }
        all.sort_by_key(|session| std::cmp::Reverse(session.modified));
        let matched = local.iter().find(|session| session.id == source_path)
            .or_else(|| local.iter().find(|session| session.id.starts_with(source_path)))
            .or_else(|| all.iter().find(|session| session.id == source_path))
            .or_else(|| all.iter().find(|session| session.id.starts_with(source_path)));
        resolved = matched.map(|session| session.path.clone()).ok_or_else(|| format!("No session found matching '{source_path}'"))?;
        resolved.as_str()
    };
    let source_entries = maho_core::session_manager::load_entries_from_file(source_path);
    if source_entries.is_empty() {
        return Err(format!("Cannot fork: source session file is empty or invalid: {source_path}"));
    }
    if !source_entries.iter().any(|entry| entry.get("type").and_then(serde_json::Value::as_str) == Some("session")) {
        return Err(format!("Cannot fork: source session has no header: {source_path}"));
    }
    let options = Some(maho_core::session_manager::NewSessionOptions {
        id: session_id.map(str::to_owned),
        parent_session: Some(source_path.to_owned()),
    });
    let mut manager = SessionManager::create(cwd, session_dir, options);
    for entry in source_entries {
        if entry.get("type").and_then(serde_json::Value::as_str) != Some("session") {
            manager.append_entry_raw(entry);
        }
    }
    Ok(manager)
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
                if let Err(error) = result { maho_core::output_guard::maho_write_stderr(&format!("{error}\n")); }
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
