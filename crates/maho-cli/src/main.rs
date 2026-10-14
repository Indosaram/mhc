fn main() {
    if let Err(error) = run() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
fn output(text: &str) -> Result<(), String> {
    use std::io::Write;
    match std::io::stdout().lock().write_all(text.as_bytes()) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}
/// senpi utils/tools-manager.ts `ensureTool`, installed as the resolver the find and grep tools use for fd/rg.
fn ensure_managed_tool(tool: maho_tools::managed_tools::ManagedTool, on_status: Option<maho_tools::managed_tools::ManagedToolStatusCallback>)
    -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<String>> + Send>> {
    use maho_cli::utils::tools_manager::{ensure_tool, Tool, ToolStatus};
    let tool = match tool { maho_tools::managed_tools::ManagedTool::Fd => Tool::Fd, maho_tools::managed_tools::ManagedTool::Rg => Tool::Rg };
    Box::pin(ensure_tool(tool, move |status: ToolStatus| {
        if let Some(on_status) = &on_status { on_status(maho_tools::managed_tools::ManagedToolStatus { warning: status.kind == "warning", message: status.message }); }
    }))
}
fn run() -> Result<(), String> {
    use std::path::PathBuf;
    maho_cli::valid_cwd::ensure_valid_cwd().map_err(|e| e.to_string())?;
    let mut argv: Vec<String> = std::env::args().skip(1).collect();
    // senpi rpc-entry.ts: the `<app>-rpc` entry is `main(["--mode", "rpc", ...argv])`; the single native
    // binary answers it when invoked through an argv0 ending in `-rpc` (a symlink or copy).
    if std::env::args_os().next().and_then(|argv0| std::path::Path::new(&argv0).file_stem().map(|stem| stem.to_string_lossy().ends_with("-rpc"))).unwrap_or(false) {
        argv.splice(0..0, ["--mode".to_owned(), "rpc".to_owned()]);
    }
    // senpi cli.ts/cli-main.ts/main.ts process setup, applied while the process is single-threaded:
    // resolve the brand before scrubbing it, mark the agent, and enter offline mode.
    let _ = maho_core::brand::brand_profile();
    let startup_env = maho_core::config::current_env();
    maho_process_env::apply(&startup_env, &maho_cli::cli::setup::startup_process_env(&argv, &startup_env));
    // senpi bun/runtime-modules.ts `registerBunRuntimeModules`: the standalone binary registers its
    // statically embedded OAuth flows once per process before any login/auth resolution.
    maho_ai::bun_oauth::register_bun_oauth_flows();
    // senpi loads its prebuilt native platform addon on demand; the Rust ports are registered once.
    maho_tui::native_platform::register_builtin_native_platform_helper();
    maho_tools::managed_tools::set_managed_tool_resolver(std::sync::Arc::new(ensure_managed_tool));
    if argv.first().is_some_and(|arg| arg == "ulw-loop") {
        let code = ulw_loop_core::cli::main_cli(&argv);
        if code != 0 { std::process::exit(code); }
        return Ok(());
    }
    let memory_role = argv.first().map(String::as_str);
    if memory_role == Some("--memory-supervisor") || memory_role == Some("--child-bootstrap") {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|error| error.to_string())?;
        let executable = std::env::current_exe().map_err(|error| error.to_string())?;
        let args: &[String] = if memory_role == Some("--memory-supervisor") { &argv[1..] } else { &argv };
        return runtime.block_on(maho_omo_memory::worker::memory_run_supervisor::run_entry(args, &executable, &[], memory_terminal_gate));
    }
    use maho_cli::experimental::process::{parse_internal_process_role, InternalProcessRole, INTERNAL_PROCESS_ENV};
    if let Some(role) = parse_internal_process_role(std::env::var(INTERNAL_PROCESS_ENV).ok().as_deref())? {
        return match role {
            InternalProcessRole::Coordinator => run_coordinator_entry(&argv),
            InternalProcessRole::Server => run_mini_server_entry(&argv),
            InternalProcessRole::SessionWorker => run_mini_worker_entry(&argv),
            InternalProcessRole::RpcSessionWorker => run_rpc_session_worker_entry(),
        };
    }
    // senpi main.ts:930: the internal supervisor route is answered before anything else runs. The
    // sentinel scan is a deliberate superset of the strict argv shape: argv that merely mentions the
    // sentinel still falls through to the public parser exactly as before.
    if argv.iter().any(|arg| arg == maho_rpc::supervisor_route::INTERNAL_SUPERVISOR_ROUTE_FLAG) {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|error| error.to_string())?;
        if runtime.block_on(maho_rpc::supervisor_route::dispatch_internal_supervisor(&argv)) {
            return Ok(());
        }
    }
    if maho_cli::cli::auth_command::is_auth_command_help(&argv) {
        return output(maho_cli::cli::auth_command::auth_command_help());
    }
    if argv.first().is_some_and(|arg| arg == "import-omo") {
        let home = std::env::var_os("HOME").ok_or("HOME is not set")?;
        let mut from = PathBuf::from(&home).join(".omo/agent");
        let to = PathBuf::from(&home).join(".maho/agent");
        let mut force = false;
        let mut args = argv.iter().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--from" => from = PathBuf::from(args.next().ok_or("--from requires a directory")?),
                "--force" => force = true,
                "--help" | "-h" => { println!("Usage: mhc import-omo [--from <directory>] [--force]\nUse --force to overwrite existing destination entries."); return Ok(()); }
                _ => return Err(format!("Unknown import option: {arg}")),
            }
        }
        let copied = maho_cli::import_omo::import_omo(&from, &to, force).map_err(|e| e.to_string())?;
        println!("Imported {} entries into {}", copied.len(), to.display());
        return Ok(());
    }
    let env = maho_core::config::current_env();
    let grok = maho_cli::cli::grok_neo_gate::is_grok_neo_enabled(&env);
    // senpi main.ts `runAuthCommand`: a parse failure or unknown option exits 1; a validation error
    // exits 2 for `auth check` (its "invalid" status code) and 1 for the credential printers.
    let command = match maho_cli::cli::auth_command::parse_auth_command(&argv) {
        Ok(command) => command,
        Err(message) => { eprintln!("Error: {message}"); std::process::exit(1); }
    };
    if let Some(command) = command {
        use maho_cli::cli::auth_command::{get_auth_command_name, get_auth_command_usage, AuthCommandKind};
        let validation_exit = if command.kind == AuthCommandKind::Check { 2 } else { 1 };
        let parsed = match maho_cli::cli::args::parse_args(&command.args, grok) {
            Ok(parsed) => parsed,
            Err(message) => { eprintln!("Error: {message}"); std::process::exit(validation_exit); }
        };
        if let Some(option) = parsed.unknown_flags.keys().next() {
            eprintln!("Unknown option --{option} for \"{}\".", get_auth_command_name(command.kind));
            eprintln!("Use \"{} --help\" or \"{}\".", maho_core::config::app_command(), get_auth_command_usage(command.kind));
            std::process::exit(1);
        }
        if !parsed.diagnostics.is_empty() {
            eprintln!("Error: {}", parsed.diagnostics.iter().map(|diagnostic| diagnostic.message.as_str()).collect::<Vec<_>>().join("\n"));
            std::process::exit(validation_exit);
        }
        if let Err(message) = maho_cli::cli::auth_command::validate_auth_command_args(&parsed, command.kind) {
            eprintln!("Error: {message}");
            std::process::exit(validation_exit);
        }
        let agent = PathBuf::from(maho_core::config::get_agent_dir());
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|error| error.to_string())?;
        return runtime.block_on(async {
            if command.kind != maho_cli::cli::auth_command::AuthCommandKind::Check {
                let model_runtime = maho_core::model_runtime::ModelRuntime::create_sync(maho_core::model_runtime::CreateModelRuntimeOptions { models_path: Some(agent.join("models.json")), auth_path: Some(agent.join("auth.json")), ..Default::default() });
                let credential = match maho_cli::cli::credential_print::resolve_credential_for_print(&parsed, &model_runtime, command.kind, command.min_expiry_ms).await {
                    Ok(credential) => credential,
                    Err(message) => { eprintln!("Error: {message}"); std::process::exit(1); }
                };
                return output(&format!("{credential}\n"));
            }
            let requested = maho_cli::cli::auth_check::requested_provider(&parsed);
            let auth_path = agent.join("auth.json");
            let model_runtime = maho_core::model_runtime::ModelRuntime::create_sync(maho_core::model_runtime::CreateModelRuntimeOptions { models_path: None, auth_path: auth_path.exists().then_some(auth_path.clone()), ..Default::default() });
            let mut result = maho_cli::cli::auth_check::check_provider_auth(&parsed, &model_runtime, !command.no_refresh).await.unwrap_or(maho_cli::cli::auth_check::AuthCheckResult { status: maho_cli::cli::auth_check::AuthCheckStatus::Invalid, provider: requested, reason: Some(maho_cli::cli::auth_check::AuthCheckReason::InvalidState), auth_type: None });
            let mut credential = None;
            if command.credentials && result.status == maho_cli::cli::auth_check::AuthCheckStatus::Ready {
                match maho_cli::cli::auth_check::provider_credential(&model_runtime, &result.provider, !command.no_refresh).await {
                    Ok(value) => {
                        credential = value;
                        if credential.is_none() { result.status = maho_cli::cli::auth_check::AuthCheckStatus::NotReady; result.reason = Some(maho_cli::cli::auth_check::AuthCheckReason::CredentialNotAvailable); }
                    }
                    // senpi: a failing credential read inside the check is an invalid auth state.
                    Err(_) => { result.status = maho_cli::cli::auth_check::AuthCheckStatus::Invalid; result.reason = Some(maho_cli::cli::auth_check::AuthCheckReason::InvalidState); result.auth_type = None; }
                }
            }
            let rendered = if command.json {
                let mut value = serde_json::to_value(&result).map_err(|error| error.to_string())?;
                // Account enrichment never turns a readable auth state into a failure.
                let mut accounts = Vec::new();
                if result.status != maho_cli::cli::auth_check::AuthCheckStatus::Invalid {
                    let storage = maho_core::auth_storage::AuthStorage::create(&auth_path.to_string_lossy());
                    let repository = maho_core::credential_pool::state_store::CredentialSlotRepository::new(
                        &maho_core::credential_pool::state_store::credential_pool_state_path(&agent.to_string_lossy()),
                    );
                    let env = |key: &str| std::env::var(key).ok();
                    let now = maho_ai::utils::diagnostics::now_ms().max(0) as u64;
                    let summaries = maho_core::credential_accounts::summarize_credential_accounts(&result.provider, storage.get(&result.provider).as_ref(), &env, &repository, now).await.unwrap_or_default();
                    accounts = summaries.iter().map(|account| {
                        let mut wire = serde_json::json!({"name": account.name, "source": account.source.as_str(), "blocked": account.blocked, "pinned": account.pinned});
                        if let Some(name) = &account.display_name { wire["displayName"] = serde_json::json!(name); }
                        wire
                    }).collect();
                }
                value["accounts"] = serde_json::Value::Array(accounts);
                if let Some(credential) = credential { value["credentials"] = serde_json::Value::String(credential); }
                serde_json::to_string(&value).map_err(|error| error.to_string())?
            } else if let Some(credential) = credential { credential } else { serde_json::to_value(&result.status).map_err(|error| error.to_string())?.as_str().unwrap_or("invalid").to_owned() };
            output(&format!("{rendered}\n"))?;
            let code = match result.status { maho_cli::cli::auth_check::AuthCheckStatus::Ready => 0, maho_cli::cli::auth_check::AuthCheckStatus::NotReady => 1, maho_cli::cli::auth_check::AuthCheckStatus::Invalid => 2 };
            if code != 0 { std::process::exit(code); }
            Ok(())
        });
    }
    // senpi main.ts:944-945: the bootstrap (project-untrusted) global `httpProxy` before package
    // commands and the runtime; still single-threaded here.
    {
        let cwd = std::env::current_dir().map_err(|error| error.to_string())?;
        let current = maho_core::config::current_env();
        let next = maho_cli::cli::setup::bootstrap_http_proxy_env(&argv, &cwd.to_string_lossy(), &maho_core::config::get_agent_dir(), &maho_core::config::home_dir(), &current)?;
        maho_process_env::apply(&current, &next);
    }
    if let Some(command) = maho_cli::package_manager_cli::parse_package_command(&argv) {
        use maho_cli::package_manager_cli::{package_command_name, package_command_usage, PackageCommand};
        if command.help { return output(&maho_cli::package_manager_cli::package_command_help(command.command)); }
        // senpi package-manager-cli.ts `handlePackageCommand`: each argument error names its problem on
        // one line and a usage hint on the next, then exits 1.
        fn usage_error(message: String, hint: String) -> ! { eprintln!("{message}"); eprintln!("{hint}"); std::process::exit(1) }
        let usage = package_command_usage(command.command);
        if let Some(option) = command.invalid_option { usage_error(format!("Unknown option {option} for \"{}\".", package_command_name(command.command)), format!("Use \"{} --help\" or \"{usage}\".", maho_core::config::app_command())); }
        if let Some(option) = command.missing_option_value { usage_error(format!("Missing value for {option}."), format!("Usage: {usage}")); }
        if let Some(argument) = command.invalid_argument { usage_error(format!("Unexpected argument {argument}."), format!("Usage: {usage}")); }
        if let Some(conflict) = command.conflicting_options { usage_error(conflict, format!("Usage: {usage}")); }
        if matches!(command.command, PackageCommand::Install | PackageCommand::Remove) && command.source.as_deref().is_none_or(str::is_empty) {
            usage_error(format!("Missing {} source.", package_command_name(command.command)), format!("Usage: {usage}"));
        }
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|error| error.to_string())?;
        return runtime.block_on(maho_cli::package_manager_cli::run_package_command(command)).map_err(|error| format!("Error: {error}"));
    }
    match argv.first().map(String::as_str) {
        Some("doctor") => {
            let code = maho_cli::cli::doctor::run_doctor(&argv[1..])?;
            if code != 0 { std::process::exit(code); }
            return Ok(());
        }
        Some("host") => {
            let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|error| error.to_string())?;
            let code = runtime.block_on(maho_cli::cli::host_command::run_host_command(&argv[1..]))?;
            if code != 0 { std::process::exit(code); }
            return Ok(());
        }
        Some("app-server") => {
            let grok = maho_cli::cli::grok_neo_gate::is_grok_neo_enabled(&maho_core::config::current_env());
            let parsed = maho_cli::cli::args::parse_args(&argv[1..], grok)?;
            let cwd = std::env::current_dir().map_err(|error| error.to_string())?;
            let agent_dir = maho_core::config::get_agent_dir();
            let config = maho_cli::cli::host_runtime::CliRuntimeConfiguration::from_parsed(
                &parsed,
                &cwd.to_string_lossy(),
                &agent_dir,
                maho_core::project_trust::AppMode::AppServer,
            );
            let session_dir = maho_cli::cli::runtime::resolve_session_dir(
                parsed.session_dir.as_deref(),
                std::env::var(maho_core::config::env_session_dir_var()).ok().as_deref(),
                None,
            );
            let executable = std::env::current_exe().map_err(|error| error.to_string())?;
            let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|error| error.to_string())?;
            let code = runtime.block_on(maho_cli::cli::app_server::run_app_server_with_signals(
                &argv,
                config,
                env!("CARGO_PKG_VERSION"),
                session_dir,
                &executable,
                &[],
            ))?;
            if code != 0 { std::process::exit(code); }
            return Ok(());
        }
        Some("config") => {
            let options = maho_cli::package_manager_cli::parse_config_command(&argv)?.ok_or("Missing config command")?;
            if options.help { return output(maho_cli::package_manager_cli::config_command_help()); }
            let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|error| error.to_string())?;
            return runtime.block_on(maho_cli::package_manager_cli::run_config_command(options));
        }
        Some("mini") => {
            let options = maho_cli::experimental::mini::entry::parse_tui_args(&argv[1..])?;
            return maho_cli::experimental::mini::entry::run_tui_entry(options);
        }
        _ => {},
    }
    let parsed = maho_cli::cli::args::parse_args(&argv, grok)?;
    if parsed.version { println!("{}", maho_core::config::display_version(env!("CARGO_PKG_VERSION"))); return Ok(()); }
    for diagnostic in &parsed.diagnostics { eprintln!("{}: {}", if diagnostic.error { "Error" } else { "Warning" }, diagnostic.message); }
    if parsed.diagnostics.iter().any(|d| d.error) { return Err("Invalid CLI arguments".to_owned()); }
    if let Some(input) = parsed.export.as_deref() {
        // senpi main.ts `parsed.export`: export the session file before any session startup.
        // `exportFromFile` resolves the default (terminal-detected) theme's colors.
        let terminal = maho_interactive::theme::detection::detect_terminal_background_from_env(std::env::var("COLORFGBG").ok().as_deref()).theme;
        let colors = maho_interactive::theme::Theme::builtin(terminal.name(), maho_interactive::theme::ColorMode::Truecolor)
            .ok().map(|theme| (theme.resolved_colors(), theme.export_colors().clone()));
        match maho_core::export_html::index::export_from_file(input, parsed.messages.first().map(String::as_str), None, colors) {
            Ok(path) => return output(&format!("Exported to: {path}\n")),
            Err(error) => {
                eprintln!("\x1b[31mError: {error}\x1b[39m");
                std::process::exit(1);
            }
        }
    }
    if parsed.mode == Some(maho_cli::cli::args::Mode::Rpc) && !parsed.file_args.is_empty() { return Err("Error: @file arguments are not supported in RPC mode".to_owned()); }
    maho_cli::cli::startup::validate_fork_flags(&parsed).map_err(|error| format!("Error: {error}"))?;
    maho_cli::cli::startup::validate_session_id_flags(&parsed).map_err(|error| format!("Error: {error}"))?;
    if std::env::var_os("MAHO_FACTS_READ_ONLY_BOOTSTRAP").is_some() {
        #[cfg(unix)] {
            let config=maho_core::lockfile_policy::facts_lock_broker_config_from_environment()?.ok_or("facts bootstrap requires a lock broker endpoint")?;
            maho_core::lockfile_policy::install_facts_lock_broker(config);
        }
        #[cfg(not(unix))]
        return Err("facts read-only bootstrap broker requires Unix sockets".into());
    }
    if parsed.help {
        // senpi cli.ts `tryPrintHelpWithoutEngine` + main.ts:1043-1058: answer plain help from the
        // cached extension flags, else load the extensions for their flags and refill the cache.
        let cwd = std::env::current_dir().map_err(|error| error.to_string())?;
        let agent = PathBuf::from(maho_core::config::get_agent_dir());
        let version = env!("CARGO_PKG_VERSION");
        use maho_cli::cli::{args::{extension_flags_text, help_text}, help_extension_flags::resolve_help_extension_flags, help_fast_path};
        if let Some(flags) = help_fast_path::cached_help_flags(&argv, grok, &cwd, &agent, version) {
            return output(&help_text(grok, &extension_flags_text(&flags)));
        }
        let resolved = resolve_help_extension_flags(&cwd, parsed.no_extensions);
        if help_fast_path::is_plain_help_request(&parsed) {
            let trusted = help_fast_path::resolve_help_project_trust(&parsed, &cwd.to_string_lossy(), &agent.to_string_lossy()).unwrap_or(false);
            let scope = help_fast_path::help_flags_scope(&parsed, &cwd, &agent, trusted);
            maho_cli::cli::help_flags_cache::write_help_flags_cache(&scope, &resolved.flags, &resolved.extension_paths, version);
        }
        return output(&help_text(grok, &extension_flags_text(&resolved.flags)));
    }
    if parsed.list_tips { return output(&format!("{}\n", serde_json::to_string_pretty(&maho_cli::cli::list_tips::collect_tips()).map_err(|e| e.to_string())?)); }
    if let Some(search) = parsed.list_models.as_deref() {
        let agent = PathBuf::from(maho_core::config::get_agent_dir());
        let cwd = std::env::current_dir().map_err(|error| error.to_string())?;
        // senpi main.ts:1030: migrations run before any runtime service, model listing included.
        maho_cli::migrations::run_migrations(&cwd, std::path::Path::new(&maho_core::config::home_dir()), &agent).map_err(|error| error.to_string())?;
        let runtime = maho_core::model_runtime::ModelRuntime::create_sync(maho_core::model_runtime::CreateModelRuntimeOptions {
            models_path: Some(agent.join("models.json")), auth_path: Some(agent.join("auth.json")), ..Default::default()
        });
        let auth_errors = runtime.credentials.errors();
        // senpi main.ts:1066-1092: model listing builds the full services, so every builtin
        // (OAuth lanes included) and every settings/`-e` extension contributes its providers.
        let runtime_host = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|error| error.to_string())?;
        let (loaded, settings) = runtime_host.block_on(async {
            let cwd_text = cwd.to_string_lossy();
            let agent_dir = maho_core::config::get_agent_dir();
            let config = maho_cli::cli::host_runtime::CliRuntimeConfiguration::from_parsed(&parsed, &cwd_text, &agent_dir, maho_core::project_trust::AppMode::Print);
            let trusted = maho_cli::cli::host_runtime::resolve_runtime_project_trust(&config, &cwd_text, config.trust_prompt_mode(), false, Box::new(|_, _| None)).await;
            let mut settings = maho_core::settings_manager::SettingsManager::create(&cwd_text, &agent_dir, &maho_core::config::home_dir(), trusted);
            let (widget_sender, _widget_requests) = tokio::sync::mpsc::unbounded_channel();
            let factories = maho_cli::cli::default_extensions::assembled_factories(widget_sender, std::sync::Arc::new(std::sync::OnceLock::new()), &settings, &cwd, &agent_dir);
            let factories = maho_cli::cli::default_extensions::with_user_extensions(factories, &mut settings, &cwd_text, &agent_dir, &config.resolved_extension_paths(), parsed.no_extensions).await?;
            Ok::<_, String>((maho_ext_host::loader::load_extensions_async(factories, &cwd, Default::default()).await, settings))
        })?;
        let providers = std::sync::Arc::new(maho_core::agent_session_runtime::ExtensionModelRuntimeActions(std::sync::Mutex::new(runtime)));
        loaded.runtime.bind_providers(providers.clone()).map_err(|error| error.message)?;
        let runtime = providers.0.lock().map_err(|error| error.to_string())?.clone();
        // senpi main.ts:1084-1089 `reportDiagnostics`: service, settings and auth diagnostics.
        for error in &loaded.errors { eprintln!("Warning: Failed to load extension \"{}\": {}", error.extension_path, error.error); }
        for diagnostic in maho_core::settings_diagnostics::collect_settings_diagnostics_with_context(&settings, "model listing") { eprintln!("Warning: {}", diagnostic.message); }
        for error in auth_errors { eprintln!("Warning: (model listing, auth storage) {error}"); }
        if let Some(error) = runtime.get_error() { eprintln!("Warning: errors loading models.json:\n{error}"); }
        return output(&format!("{}\n", maho_cli::cli::list_models::list_models(&runtime, Some(search))));
    }
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|error| error.to_string())?;
    runtime.block_on(maho_cli::cli::runtime::run(parsed, &argv))
}
fn memory_terminal_gate(directory: &std::path::Path, operation: &mut dyn FnMut() -> Result<(), String>) -> Result<(), String> {
    let record = memory_core::locks::create_lock_record("reflection-finalize", Default::default()).map_err(|error| error.to_string())?;
    let path = directory.join("terminalization.lock");
    loop {
        match memory_core::locks::acquire_lock(&path, &record, &memory_core::locks::AcquireLockOptions { wait_timeout_ms: Some(60_000), ..Default::default() }) {
            Ok(()) => break,
            Err(memory_core::locks::AcquireLockError::Contention(_)) => continue,
            Err(error) => return Err(error.to_string()),
        }
    }
    let result = operation();
    memory_core::locks::release_lock(&path, &record).map_err(|error| error.to_string())?;
    result
}
#[cfg(unix)]
fn run_coordinator_entry(argv: &[String]) -> Result<(), String> {
    let [public, control, ..] = argv else { return Err("Coordinator requires public and control socket paths".to_owned()); };
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|error| error.to_string())?;
    runtime.block_on(async {
        let shutdown = maho_ai::utils::abort::AbortController::new();
        let signal = shutdown.signal();
        let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt()).map_err(|error| error.to_string())?;
        let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).map_err(|error| error.to_string())?;
        let coordinator = maho_cli::experimental::coordinator::run_coordinator(std::path::Path::new(public), std::path::Path::new(control), &signal);
        tokio::pin!(coordinator);
        tokio::select! { result = &mut coordinator => result, _ = interrupt.recv() => { shutdown.abort(None); coordinator.await }, _ = terminate.recv() => { shutdown.abort(None); coordinator.await } }
    })
}
#[cfg(not(unix))]
fn run_coordinator_entry(_argv: &[String]) -> Result<(), String> { Err("Coordinator named-pipe transport has not been ported on this platform".to_owned()) }
#[cfg(unix)]
fn run_mini_server_entry(argv: &[String]) -> Result<(), String> {
    use maho_cli::experimental::mini::{entry::{parse_server_args, run_server_entry}, server::process_worker_factory};
    let options = parse_server_args(argv)?;
    let cwd = std::env::current_dir().map_err(|error| error.to_string())?;
    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    let spawn = process_worker_factory(executable, Vec::new(), options.sessions_root.clone());
    run_server_entry(options, &cwd.to_string_lossy(), spawn)
}
#[cfg(not(unix))]
fn run_mini_server_entry(_argv: &[String]) -> Result<(), String> { Err("The mini session server requires the Unix socket transport".to_owned()) }
#[cfg(unix)]
fn run_mini_worker_entry(argv: &[String]) -> Result<(), String> {
    use maho_cli::experimental::mini::entry::{parse_worker_args, run_worker_entry};
    run_worker_entry(parse_worker_args(argv)?)
}
#[cfg(not(unix))]
fn run_mini_worker_entry(_argv: &[String]) -> Result<(), String> { Err("The mini session worker requires the Unix socket transport".to_owned()) }
/// senpi \`modes/rpc/session-worker.ts\`: one shared-host session in an isolated process. Stdout is the
/// protocol channel, so every stray write is routed to stderr first (senpi \`takeOverStdout()\`).
fn run_rpc_session_worker_entry() -> Result<(), String> {
    maho_cli::cli::setup::register_builtin_apis();
    maho_core::output_guard::take_over_stdout();
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|error| error.to_string())?;
    runtime.block_on(maho_rpc::session_worker::run_session_worker(Box::new(maho_cli::cli::host_runtime::session_worker_configuration)))
}
