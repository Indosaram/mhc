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
fn run() -> Result<(), String> {
    use std::path::PathBuf;
    maho_cli::valid_cwd::ensure_valid_cwd().map_err(|e| e.to_string())?;
    let argv: Vec<String> = std::env::args().skip(1).collect();
    use maho_cli::experimental::process::{parse_internal_process_role, InternalProcessRole, INTERNAL_PROCESS_ENV};
    if let Some(role) = parse_internal_process_role(std::env::var(INTERNAL_PROCESS_ENV).ok().as_deref())? {
        return match role {
            InternalProcessRole::Coordinator => run_coordinator_entry(&argv),
            InternalProcessRole::Server | InternalProcessRole::SessionWorker => Err("Experimental server and session-worker entrypoints require excluded chord runtime (D-M5)".to_owned()),
        };
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
    if let Some(command) = maho_cli::cli::auth_command::parse_auth_command(&argv)? {
        let parsed = maho_cli::cli::args::parse_args(&command.args, grok)?;
        maho_cli::cli::auth_command::validate_auth_command_args(&parsed, command.kind)?;
        return Err("Auth execution blocked: maho-core ModelRuntime check_auth/list_credentials/model-aware get_auth API request (todo 16)".to_owned());
    }
    if let Some(command) = maho_cli::package_manager_cli::parse_package_command(&argv) {
        if command.help { return output(&maho_cli::package_manager_cli::package_command_help(command.command)); }
        if let Some(option) = command.invalid_option { return Err(format!("Unknown package option: {option}")); }
        if let Some(argument) = command.invalid_argument { return Err(format!("Unexpected package argument: {argument}")); }
        if let Some(option) = command.missing_option_value { return Err(format!("{option} requires a value")); }
        if let Some(conflict) = command.conflicting_options { return Err(conflict); }
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|error| error.to_string())?;
        return runtime.block_on(maho_cli::package_manager_cli::run_package_command(command));
    }
    match argv.first().map(String::as_str) {
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
            let executable = std::env::current_exe().map_err(|error| error.to_string())?;
            let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|error| error.to_string())?;
            let code = runtime.block_on(maho_cli::cli::app_server::run_app_server_with_signals(
                &argv,
                config,
                env!("CARGO_PKG_VERSION"),
                parsed.session_dir.clone(),
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
        _ => {},
    }
    let parsed = maho_cli::cli::args::parse_args(&argv, grok)?;
    if parsed.version { println!("{}", maho_core::config::display_version(env!("CARGO_PKG_VERSION"))); return Ok(()); }
    for diagnostic in &parsed.diagnostics { eprintln!("{}: {}", if diagnostic.error { "Error" } else { "Warning" }, diagnostic.message); }
    if parsed.diagnostics.iter().any(|d| d.error) { return Err("Invalid CLI arguments".to_owned()); }
    if parsed.mode == Some(maho_cli::cli::args::Mode::Rpc) && !parsed.file_args.is_empty() { return Err("Error: @file arguments are not supported in RPC mode".to_owned()); }
    maho_cli::cli::startup::validate_fork_flags(&parsed).map_err(|error| format!("Error: {error}"))?;
    maho_cli::cli::startup::validate_session_id_flags(&parsed).map_err(|error| format!("Error: {error}"))?;
    if parsed.help { return output(&maho_cli::cli::args::help_text(grok, "")); }
    if parsed.list_tips { return output(&format!("{}\n", serde_json::to_string_pretty(&maho_cli::cli::list_tips::collect_tips()).map_err(|e| e.to_string())?)); }
    if let Some(search) = parsed.list_models.as_deref() {
        let agent = PathBuf::from(maho_core::config::get_agent_dir());
        let runtime = maho_core::model_runtime::ModelRuntime::create_sync(maho_core::model_runtime::CreateModelRuntimeOptions {
            models_path: Some(agent.join("models.json")), auth_path: Some(agent.join("auth.json")), ..Default::default()
        });
        let cwd = std::env::current_dir().map_err(|error| error.to_string())?;
        let loaded = maho_cli::cli::extension_registry::load_native_extensions(&cwd, Default::default());
        let providers = std::sync::Arc::new(maho_core::agent_session_runtime::ExtensionModelRuntimeActions(std::sync::Mutex::new(runtime)));
        loaded.runtime.bind_providers(providers.clone()).map_err(|error| error.message)?;
        let runtime = providers.0.lock().map_err(|error| error.to_string())?.clone();
        for error in &loaded.errors { eprintln!("{}: {}", error.extension_path, error.error); }
        if runtime.get_error().is_some() { eprintln!("Warning: errors loading models.json"); }
        return output(&format!("{}\n", maho_cli::cli::list_models::list_models(&runtime, Some(search))));
    }
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|error| error.to_string())?;
    runtime.block_on(maho_cli::cli::runtime::run(parsed, &argv))
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
