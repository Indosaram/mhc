use super::{args::Args, startup};
use maho_core::{project_trust::AppMode, session_manager::SessionManager};
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
        let code = super::app_server::run_app_server_with_signals(argv, config, env!("CARGO_PKG_VERSION"), resolve_session_dir(parsed.session_dir.as_deref(), std::env::var(maho_core::config::env_session_dir_var()).ok().as_deref(), None), &executable, &[]).await?;
        if code != 0 { return Err(format!("app-server exited with code {code}")); }
        return Ok(());
    }
    if mode != AppMode::Interactive {
        super::stdout_guard::take_over_stdout();
        // senpi main.ts:1008-1012: route process-wide stdout writes (`maho_write_stdout`) to stderr
        // so stray output cannot corrupt the JSON/RPC stream.
        maho_core::output_guard::take_over_stdout();
    }
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
    if mode == AppMode::Rpc && parsed.multi_session {
        // senpi main.ts:1119 `resolveSessionRuntime`: stdio hosts default to one isolated worker per
        // session. The native host has no worker session registry yet, so a worker runtime
        // degrades to the in-process registry; say so rather than silently ignoring the request.
        if super::args::resolve_session_runtime(&parsed) == super::args::SessionRuntime::Worker {
            maho_core::output_guard::maho_write_stderr("Warning: --session-runtime worker is not supported by the native host; sessions run in-process.\n");
        }
        // senpi main.ts:1112-1154: a multi-session RPC host serves every session in this process,
        // built from the CLI configuration, instead of the single-session command stream. Like
        // upstream it starts only after the stdout takeover and the migrations above, and never
        // returns until its watchdog or empty-exit policy ends it.
        let mut config = super::host_runtime::CliRuntimeConfiguration::from_parsed(&parsed, &cwd_text, &agent_dir, AppMode::Rpc);
        config.session_dir = resolve_session_dir(parsed.session_dir.as_deref(), std::env::var(maho_core::config::env_session_dir_var()).ok().as_deref(), None);
        let core = super::host_runtime::create_host_core(config, &cwd_text, parsed.listen.as_deref());
        return super::host_runtime::serve_multi_session_host(core, parsed.listen.clone()).await.map_err(|error| error.to_string());
    }
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
    let (project_trusted, project_trust_warnings) = indicator.during_prompt(super::host_runtime::resolve_runtime_project_trust_with_warnings(
        &trust_config, &cwd_text, trust_mode, trust_mode == AppMode::Interactive, selector,
    )).await;
    let mut settings = maho_core::settings_manager::SettingsManager::create(&cwd_text, &agent_dir, &maho_core::config::home_dir(), project_trusted);
    // senpi main.ts:769-775 `projectTrustDiagnostics`, then `reportDiagnostics(collectSettingsDiagnosticsWithContext(..., "runtime creation"))`.
    let mut runtime_diagnostics: Vec<_> = project_trust_warnings.into_iter().map(maho_core::settings_diagnostics::AgentSessionRuntimeDiagnostic::warning).collect();
    runtime_diagnostics.extend(maho_core::settings_diagnostics::collect_settings_diagnostics_with_context(&settings, "runtime creation"));
    // senpi main.ts:1254: re-apply the global `httpProxy` from the session settings (post-migration).
    // `apply` only writes keys that changed, and the bootstrap pass already set an unchanged proxy.
    {
        let current = maho_core::config::current_env();
        let mut next = current.clone();
        let proxy = settings.get_global().get("httpProxy").and_then(serde_json::Value::as_str).map(str::to_owned);
        maho_core::http_dispatcher::apply_http_proxy_settings(proxy.as_deref(), parsed.multi_session, &mut next)?;
        maho_process_env::apply(&current, &next);
    }
    // senpi `ModelRuntime.create`: restores the stored dynamic catalogs (offline) before use.
    let models = maho_core::model_runtime::ModelRuntime::create(maho_core::model_runtime::CreateModelRuntimeOptions {
        models_path: Some(std::path::Path::new(&agent_dir).join("models.json")),
        auth_path: Some(std::path::Path::new(&agent_dir).join("auth.json")),
        agent_dir: Some(std::path::PathBuf::from(&agent_dir)), ..Default::default()
    }).await;
    let identity = Some(maho_core::session_manager::NewSessionOptions { id: parsed.session_id.clone(), ..Default::default() });
    let session_dir = resolve_session_dir(parsed.session_dir.as_deref(), std::env::var(maho_core::config::env_session_dir_var()).ok().as_deref(), settings.get_string("sessionDir").as_deref());
    let mut manager = if parsed.no_session { SessionManager::in_memory(&cwd_text, identity, None) }
        else if let Some(source) = &parsed.fork { fork_session(source, &cwd_text, session_dir.as_deref(), parsed.session_id.as_deref())? }
        else if let Some(argument) = &parsed.session {
            // senpi main.ts:477-513: a path opens as-is, an id/prefix resolves locally then globally.
            match resolve_session_path(argument, &cwd_text, session_dir.as_deref()) {
                ResolvedSession::Path(path) | ResolvedSession::Local(path) => SessionManager::open(&path, session_dir.as_deref(), None, None),
                ResolvedSession::Global { path, cwd: session_cwd } => {
                    if mode != AppMode::Interactive {
                        return Err(format!("Session found in different project: {session_cwd}\nCannot confirm forking without an interactive session. Use --fork '{argument}' to fork it into the current directory, or re-run interactively from {session_cwd}."));
                    }
                    let fork = indicator.during_surface_write(|| {
                        println!("\x1b[33mSession found in different project: {session_cwd}\x1b[39m");
                        prompt_confirm("Fork this session into current directory?")
                    });
                    if !fork {
                        indicator.stop();
                        println!("\x1b[2mAborted.\x1b[22m");
                        std::process::exit(0);
                    }
                    fork_session(&path, &cwd_text, session_dir.as_deref(), None)?
                }
                ResolvedSession::NotFound(argument) => return Err(format!("No session found matching '{argument}'")),
            }
        }
        else if parsed.resume {
            // senpi main.ts:515-531: the startup session picker; cancelling exits 0.
            match indicator.during_surface_write(|| super::session_picker::select_session(&cwd_text, session_dir.as_deref(), &settings))? {
                Some(path) => SessionManager::open(&path, session_dir.as_deref(), None, None),
                None => {
                    indicator.stop();
                    println!("\x1b[2mNo session selected\x1b[22m");
                    std::process::exit(0);
                }
            }
        }
        else if parsed.continue_session { SessionManager::continue_recent(&cwd_text, session_dir.as_deref()) }
        else if let Some(id) = parsed.session_id.as_deref() {
            // senpi main.ts:537-547: an existing exact-id project session is resumed, not duplicated.
            match find_local_session_by_exact_id(id, &cwd_text, session_dir.as_deref()) {
                Some(path) => SessionManager::open(&path, session_dir.as_deref(), None, None),
                None => {
                    indicator.during_surface_write(|| eprintln!("\x1b[33mWarning: No project session found with id '{id}'; creating a new session with that id.\x1b[39m"));
                    SessionManager::create(&cwd_text, session_dir.as_deref(), identity)
                }
            }
        }
        else { SessionManager::create(&cwd_text, session_dir.as_deref(), identity) };
    // senpi main.ts:1168-1180: a resumed session whose stored cwd is gone continues in the launch
    // cwd after an interactive confirmation, and is an error in every other mode.
    if let Some(issue) = maho_core::session_cwd::get_missing_session_cwd_issue(&manager, &cwd_text) {
        if mode != AppMode::Interactive { return Err(maho_core::session_cwd::format_missing_session_cwd_error(&issue)); }
        let options = vec!["Continue".to_owned(), "Cancel".to_owned()];
        let (theme, hardware_cursor, clear_on_shrink) = (settings.get_string("theme"), settings.get_bool("showHardwareCursor").unwrap_or(false), settings.get_bool("clearOnShrink").unwrap_or(false));
        let prompt = maho_core::session_cwd::format_missing_session_cwd_prompt(&issue);
        let choice = indicator.during_surface_write(|| super::startup_ui::show_startup_selector(theme.as_deref(), hardware_cursor, clear_on_shrink, &prompt, &options))?;
        let session_file = issue.session_file.clone().filter(|_| choice.as_deref() == Some("Continue"));
        let Some(session_file) = session_file else {
            indicator.stop();
            std::process::exit(0);
        };
        manager = SessionManager::open(&session_file, session_dir.as_deref(), Some(issue.fallback_cwd.as_str()), None);
    }
    let has_context_messages = manager.has_context_messages();
    // senpi main.ts:1181-1188: a blank `--name` is rejected before the runtime is built.
    let session_name = match parsed.name.as_deref() {
        Some(raw) => Some(super::args::normalize_session_name(raw).ok_or("Error: --name requires a non-empty value")?.to_owned()),
        None => None,
    };
    let (widget_sender, widget_requests) = tokio::sync::mpsc::unbounded_channel();
    let task_parent = Arc::new(std::sync::OnceLock::new());
    // Builtins with the OAuth provider lanes, filtered by the builtin-extension settings selection.
    let base_factories = indicator.during_surface_write(|| super::default_extensions::assembled_factories(widget_sender, task_parent.clone(), &settings, &cwd, &agent_dir));
    // senpi `collectExtensionLoadDiagnostics` (main.ts:129-136): an extension that cannot load is a
    // warning; startup continues without it. `--no-extensions` keeps only the `-e` paths.
    let (base_factories, extension_errors) = super::default_extensions::with_user_extensions(base_factories, &mut settings, &cwd_text, &agent_dir, &parsed.extensions, parsed.no_extensions).await?;
    runtime_diagnostics.extend(extension_errors.into_iter().map(|error| maho_core::settings_diagnostics::AgentSessionRuntimeDiagnostic::warning(super::default_extensions::extension_load_failure_message(&error.path, &error.message))));
    let session_profile = super::shared_host::session_profile(mode, &settings, None, None);
    let extensions = indicator.during_prompt(maho_ext_host::loader::load_extensions_async(base_factories.clone(), &cwd, session_profile.clone())).await;
    let extension_snapshot = extensions.extensions.clone();
    // senpi main.ts:1262-1268: every full launch refreshes what `--help` reads.
    super::help_flags_cache::write_help_flags_cache(
        &super::help_fast_path::help_flags_scope(&parsed, &cwd, std::path::Path::new(&agent_dir), settings.is_project_trusted()),
        &extensions.extensions.iter().flat_map(|extension| extension.flags.iter().map(super::help_extension_flags::help_flag_value)).collect::<Vec<_>>(),
        &extensions.extensions.iter().map(|extension| std::path::PathBuf::from(&extension.identity.resolved_path)).collect::<Vec<_>>(),
        env!("CARGO_PKG_VERSION"),
    );
    runtime_diagnostics.extend(extensions.errors.iter().map(|error| maho_core::settings_diagnostics::AgentSessionRuntimeDiagnostic::warning(format!("Failed to load extension \"{}\": {}", error.extension_path, error.error))));
    if mode != AppMode::Interactive {
        runtime_diagnostics.extend(models.credentials.errors().into_iter().map(|error| maho_core::settings_diagnostics::AgentSessionRuntimeDiagnostic::warning(format!("(runtime creation, auth storage) {error}"))));
    }
    indicator.during_surface_write(|| { for diagnostic in &runtime_diagnostics { maho_core::output_guard::maho_write_stderr(&format!("Warning: {}\n", diagnostic.message)); } });
    let extension_load_failed = runtime_diagnostics.iter().any(|diagnostic| diagnostic.message.contains("Failed to load extension"));
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
    // senpi `main.ts` `resolveModelScope` with `getModelNarrowingPatterns({ cliPatterns, legacyEnabledPatterns:
    // settings.getEnabledModels() })` (main.ts:825-828): scope patterns resolve against authenticated models.
    let enabled_models: Option<Vec<String>> = settings.get_value("enabledModels").and_then(|value| serde_json::from_value(value.clone()).ok());
    let model_patterns = maho_core::model_resolver::get_model_narrowing_patterns(parsed.models.as_deref(), enabled_models.as_deref());
    let scope_models = if model_patterns.is_empty() { Vec::new() } else { models.get_available(None).await };
    let scope = maho_core::model_resolver::resolve_model_scope_from_models(model_patterns, &scope_models);
    let mut options = startup::build_session_options(&parsed, &scope.scoped_models, has_context_messages, &models, &settings);
    // senpi `main.ts`: `--api-key` is a runtime key for the selected model's provider.
    if let Some(api_key) = &parsed.api_key {
        match &options.options.model {
            None => options.diagnostics.push(super::args::Diagnostic { error: true, message: "--api-key requires a model to be specified via --model, --provider/--model, or --models".into() }),
            Some(model) => models.set_runtime_api_key(&model.provider, api_key, Default::default()).await.map_err(|error| error.to_string())?,
        }
    }
    indicator.during_surface_write(|| { for diagnostic in &options.diagnostics { maho_core::output_guard::maho_write_stderr(&format!("{}: {}\n", if diagnostic.error { "Error" } else { "Warning" }, diagnostic.message)); } });
    if options.diagnostics.iter().any(|diagnostic| diagnostic.error) {
        // senpi main.ts EXTENSION_LOAD_FAILURE_HINT: an error exit after an extension failed to load
        // points at the extension-free launch.
        if extension_load_failed || options.diagnostics.iter().any(|diagnostic| diagnostic.message.contains("Failed to load extension")) {
            maho_core::output_guard::maho_write_stderr(&format!("Hint: Start without extensions using \"{} -ne\".\n", maho_core::config::app_command()));
        }
        return Err("Invalid model selection".into());
    }
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
        session_profile,
        auto_title_sessions: Some(startup::resolve_auto_title_sessions(
            mode, &parsed, has_context_messages,
            &maho_rpc::custom_capability::parse_client_capabilities(maho_core::brand::env_value("RPC_CLIENT_CAPABILITIES", &maho_core::config::current_env()).as_deref()),
            None,
        )),
        resource_inputs: maho_core::resource_loader::ResourceLoaderInputs {
            additional_extension_paths: parsed.extensions.clone(), additional_skill_paths: parsed.skills.clone(),
            additional_prompt_template_paths: parsed.prompt_templates.clone(), no_skills: parsed.no_skills,
            no_prompt_templates: parsed.no_prompt_templates, ..Default::default()
        },
        ..Default::default()
    }).await.map_err(|error| if mode != AppMode::Interactive && error == "No model available" { maho_core::auth_guidance::format_no_models_available_message() } else { error })?;
    // senpi main.ts:1292-1295: print/json/rpc without a usable model exits 1 with the login help.
    let created_model = created.session.model();
    if mode != AppMode::Interactive && created_model.provider == "unknown" && created_model.id == "unknown" {
        return Err(maho_core::auth_guidance::format_no_models_available_message());
    }
    let session = Arc::new(created.session);
    // senpi main.ts:1253: `setCapabilityOverrides(settingsManager.getTerminalCapabilityOverrides())`.
    maho_tui::terminal_image::set_capability_overrides(session.with_settings_manager(|settings| settings.get_terminal_capability_overrides(&maho_core::config::current_env())));
    let model_fallback_message = created.model_fallback_message;
    task_parent.set(session.weak_accessor()).map_err(|_| "Task parent already bound".to_owned())?;
    session.set_context_files_enabled(!parsed.no_context_files);
    let runtime_config = super::host_runtime::CliRuntimeConfiguration::from_parsed(&parsed, &cwd_text, &agent_dir, mode);
    {
        // senpi export-html resolves theme colors from the configured theme in every mode; the
        // single-session print/RPC path installs the same resolver as the shared host runtime.
        let mut theme_paths = Vec::new();
        let session_cwd = session.cwd();
        let session_agent_dir = session.agent_dir();
        let mut theme_settings = maho_core::settings_manager::SettingsManager::in_memory(session.with_settings_manager(|settings| settings.get_global().clone()), false);
        let packages = maho_core::package_manager::DefaultPackageManager::new(maho_core::package_manager::PackageManagerOptions { cwd: &session_cwd, agent_dir: &session_agent_dir, settings_manager: &mut theme_settings });
        match packages.resolve(None).await {
            Ok(paths) => theme_paths.extend(paths.themes.into_iter().filter(|resource| resource.enabled).map(|resource| std::path::PathBuf::from(resource.path))),
            Err(error) => { maho_core::output_guard::maho_write_stderr(&format!("package themes: {error}\n")); }
        }
        theme_paths.extend(runtime_config.resolved_theme_paths().into_iter().map(std::path::PathBuf::from));
        super::default_extensions::configure_rpc_html_theme_with_paths(&session, theme_paths);
    }
    let hook_sources = session.with_settings_manager(|settings| super::hook_sources::build_loaded_hook_sources(&runtime_config, settings));
    session.set_hook_sources(Some(hook_sources));
    if parsed.no_skills || parsed.no_prompt_templates || !parsed.skills.is_empty() || !parsed.prompt_templates.is_empty() {
        let skill_diagnostics = session.loaded_skills().unwrap_or_default().diagnostics;
        let prompt_diagnostics = session.prompt_diagnostics();
        indicator.during_surface_write(|| { for diagnostic in skill_diagnostics.iter().chain(&prompt_diagnostics) { maho_core::output_guard::maho_write_stderr(&format!("{}\n", diagnostic.message)); } });
    }
    if let Some(level) = options.options.thinking_level { session.set_session_thinking_level(level); }
    if let Some(name) = &session_name { session.set_session_name(name); }
    indicator.stop();
    if mode == AppMode::Interactive { crate::migrations::show_deprecation_warnings(&migrations.deprecation_warnings).map_err(|error| error.to_string())?; }
    let run = async { match mode {
        AppMode::Rpc => {
            // senpi main.ts:1324-1332: RPC refreshes model catalogs in the background, bounded to 15 s
            // and skipped offline; interactive mode starts its own refresh after TUI startup.
            let offline = parsed.offline || super::shared_host::is_truthy_env_flag(maho_core::brand::env_value("OFFLINE", &maho_core::config::current_env()).as_deref());
            if !offline {
                let mut runtime = session.model_runtime().clone();
                tokio::spawn(async move {
                    let _ = tokio::time::timeout(std::time::Duration::from_secs(15), runtime.refresh(maho_ai::models::ModelsRefreshOptions::default())).await;
                });
            }
            // The connection handler performs the first extension bind with the RPC UI context
            // (senpi `createRpcConnectionHandler` -> `rebindSession`), so extensions see `hasUI`.
            let code = maho_rpc::rpc_mode::run_rpc_mode(&session, tokio::io::stdin(), tokio::io::stdout()).await.map_err(|error| error.to_string())?;
            // senpi `createRpcShutdown`: SIGTERM/SIGHUP (143/129) kill tracked detached children.
            #[cfg(unix)]
            if code == 143 || code == 129 { crate::utils::shell::kill_tracked_detached_children(); }
            Ok(code)
        },
        AppMode::Print | AppMode::Json => {
            session.bind_extensions(maho_core::agent_session::ExtensionBindings {
                mode: Some(if mode == AppMode::Json { maho_ext_api::ExtensionMode::Json } else { maho_ext_api::ExtensionMode::Print }),
                // senpi print-mode.ts:123-125.
                on_error: Some(Arc::new(|error: &maho_ext_api::ExtensionError| {
                    maho_core::output_guard::maho_write_stderr(&format!("Extension error ({}): {}\n", error.extension_path, error.error));
                })),
                ..Default::default()
            }).await;
            use tokio::io::AsyncReadExt;
            let stdin = if stdin_tty { None } else { let mut text = String::new(); tokio::io::stdin().read_to_string(&mut text).await.map_err(|error| error.to_string())?; Some(text) };
            let initial = startup::prepare_initial_message(&mut parsed, &cwd, true, stdin.as_deref()).await?;
            // senpi print-mode.ts `runPrintMode`: the session header, session events (not raw agent
            // events) as JSON lines, fallback diagnostics and the text result all come from the
            // shared native print runner.
            let initial = initial.initial_message.map(|text| (text, initial.initial_images.unwrap_or_default()));
            maho_rpc::print_mode::run_print_session(&session, mode == AppMode::Json, initial, &parsed.messages, tokio::io::stdout(), tokio::io::stderr())
                .await.map_err(|error| error.to_string())
                .inspect(|_| {
                    // senpi main.ts:1385 `collectAuthDiagnostics(services.authStorage, "print mode")`.
                    for error in session.model_runtime().credentials.drain_errors() {
                        maho_core::output_guard::maho_write_stderr(&format!("Warning: (print mode, auth storage) {error}\n"));
                    }
                    // senpi main.ts:1387.
                    maho_core::output_guard::restore_stdout();
                })
        }
        AppMode::Interactive => {
            let initial = startup::prepare_initial_message(&mut parsed, &cwd, true, None).await?;
            super::interactive_entry::run(session.clone(), &parsed, initial, extension_snapshot, widget_requests, migrations.migrated_auth_providers.clone(), model_fallback_message.clone()).await.map(|()| 0)
        },
        AppMode::AppServer => unreachable!("server mode rejected before session creation"),
    } };
    // senpi print-mode.ts:55-73 / rpc-mode.ts:150-162: SIGTERM/SIGHUP in print, json and RPC mode
    // kill tracked detached children, run `session_shutdown` and dispose, then exit 143/129.
    // RPC mode owns its SIGTERM/SIGHUP handling inside `run_rpc_mode` (connection release first) and
    // returns 143/129 as its exit code.
    #[cfg(unix)]
    let (result, signal_code) = if matches!(mode, AppMode::Interactive | AppMode::Rpc) { (run.await, None) } else {
        use tokio::signal::unix::{signal, SignalKind};
        let mut terminate = signal(SignalKind::terminate()).map_err(|error| error.to_string())?;
        let mut hangup = signal(SignalKind::hangup()).map_err(|error| error.to_string())?;
        tokio::select! {
            result = run => (result, None),
            _ = terminate.recv() => (Ok(143), Some(143)),
            _ = hangup.recv() => (Ok(129), Some(129)),
        }
    };
    #[cfg(not(unix))]
    let (result, signal_code): (Result<i32, String>, Option<i32>) = (run.await, None);
    #[cfg(unix)]
    { if signal_code.is_some() { crate::utils::shell::kill_tracked_detached_children(); } }
    session.emit_session_shutdown(maho_ext_api::SessionReason::Quit).await;
    session.dispose().await;
    if let Some(code) = signal_code { std::process::exit(code); }
    let exit_code = result?;
    if exit_code != 0 { std::process::exit(exit_code); }
    Ok(())
}

/// senpi `ResolvedSession`.
#[derive(Debug, PartialEq, Eq)]
pub enum ResolvedSession {
    Path(String),
    Local(String),
    Global { path: String, cwd: String },
    NotFound(String),
}

/// senpi `findLocalSessionByExactId`.
pub fn find_local_session_by_exact_id(session_id: &str, cwd: &str, session_dir: Option<&str>) -> Option<String> {
    let sessions = if session_dir.is_some() { SessionManager::list_all(session_dir, None) } else { SessionManager::list(cwd, None, None) };
    sessions.into_iter().find(|session| session.id == session_id).map(|session| session.path)
}

/// senpi `resolveSessionPath`: a path is resolved against `cwd`; otherwise the argument is an id or
/// id prefix matched in this project first, then across every project.
pub fn resolve_session_path(session_arg: &str, cwd: &str, session_dir: Option<&str>) -> ResolvedSession {
    if session_arg.contains('/') || session_arg.contains('\\') || session_arg.ends_with(".jsonl") {
        return ResolvedSession::Path(maho_core::paths::resolve_path(session_arg, cwd, &maho_core::paths::PathInputOptions::expanding_tilde(maho_core::config::home_dir())));
    }
    let matching = |sessions: &[maho_core::session_discovery::SessionInfo]| {
        sessions.iter().find(|session| session.id == session_arg)
            .or_else(|| sessions.iter().find(|session| session.id.starts_with(session_arg)))
            .map(|session| (session.path.clone(), session.cwd.clone()))
    };
    if let Some((path, _)) = matching(SessionManager::list(cwd, session_dir, None).as_slice()) { return ResolvedSession::Local(path); }
    if let Some((path, cwd)) = matching(SessionManager::list_all(session_dir, None).as_slice()) { return ResolvedSession::Global { path, cwd }; }
    ResolvedSession::NotFound(session_arg.to_owned())
}

/// senpi `promptConfirm`: `[y/N]` on stdin; EOF or a read error answers no.
fn prompt_confirm(message: &str) -> bool {
    use std::io::Write;
    print!("{message} [y/N] ");
    if std::io::stdout().flush().is_err() { return false; }
    let mut answer = String::new();
    match std::io::stdin().read_line(&mut answer) {
        Ok(0) | Err(_) => false,
        Ok(_) => matches!(answer.trim().to_lowercase().as_str(), "y" | "yes"),
    }
}

/// senpi main.ts:1162-1166: `--session-dir` (path-normalized), then `<PREFIX>_CODING_AGENT_SESSION_DIR`
/// (tilde-expanded), then the `sessionDir` setting (path-normalized). Empty values count as unset.
pub fn resolve_session_dir(flag: Option<&str>, env: Option<&str>, setting: Option<&str>) -> Option<String> {
    let options = maho_core::paths::PathInputOptions::expanding_tilde(maho_core::config::home_dir());
    [flag, env, setting].into_iter().flatten().find(|value| !value.is_empty())
        .map(|value| maho_core::paths::normalize_path(value, &options))
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
    SessionManager::fork_from(source_path, cwd, session_dir, Some(maho_core::session_manager::NewSessionOptions {
        id: session_id.map(str::to_owned),
        parent_session: None,
    }))
}
