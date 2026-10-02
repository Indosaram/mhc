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
                "--help" | "-h" => { println!("Usage: mhc import-omo [--from <directory>] [--force]"); return Ok(()); }
                _ => return Err(format!("Unknown import option: {arg}")),
            }
        }
        let copied = maho_cli::import_omo::import_omo(&from, &to, force).map_err(|e| e.to_string())?;
        println!("Imported {} entries into {}", copied.len(), to.display());
        return Ok(());
    }
    let env = maho_core::config::current_env();
    let grok = maho_core::brand::env_value("ENABLE_GROK_NEO", &env).is_some_and(|value| matches!(value.trim().to_lowercase().as_str(), "1" | "true" | "yes"));
    let parsed = maho_cli::cli::args::parse_args(&argv, grok)?;
    if parsed.version { println!("{}", maho_core::config::display_version(env!("CARGO_PKG_VERSION"))); return Ok(()); }
    for diagnostic in &parsed.diagnostics { eprintln!("{}: {}", if diagnostic.error { "Error" } else { "Warning" }, diagnostic.message); }
    if parsed.diagnostics.iter().any(|d| d.error) { return Err("Invalid CLI arguments".to_owned()); }
    if parsed.help { return output(&maho_cli::cli::args::help_text(grok, "")); }
    if parsed.list_tips { return output(&format!("{}\n", serde_json::to_string_pretty(&maho_cli::cli::list_tips::collect_tips()).map_err(|e| e.to_string())?)); }
    if let Some(search) = parsed.list_models.as_deref() {
        let agent = PathBuf::from(maho_core::config::get_agent_dir());
        let runtime = maho_core::model_runtime::ModelRuntime::create_sync(maho_core::model_runtime::CreateModelRuntimeOptions {
            models_path: Some(agent.join("models.json")), auth_path: Some(agent.join("auth.json")), ..Default::default()
        });
        if runtime.get_error().is_some() { eprintln!("Warning: errors loading models.json"); }
        return output(&format!("{}\n", maho_cli::cli::list_models::list_models(&runtime, Some(search))));
    }
    Err("Requested mode is not yet available: interactive todo 35, RPC todo 36, server todo 37; native extension assembly todo 48".to_owned())
}
