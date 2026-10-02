use maho_server::app_server::{cli_args::{CliArgs, parse_cli_args}, index::run_app_server_mode, runtime::AppServerRuntime};

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
        let started = run_daemon_command(&paths,DaemonVerb::Start,&listen,"1",&executable,&[]).await?;
        let status = run_daemon_command(&paths,DaemonVerb::Status,&listen,"1",&executable,&[]).await;
        let stopped = run_daemon_command(&paths,DaemonVerb::Stop,&listen,"1",&executable,&[]).await?;
        let status = status?;
        if started["status"] != "started" || status["status"] != "running" || stopped["status"] != "stopped" || socket.exists() || paths.pid_file.exists() || paths.settings_file.exists() {return Err("Daemon QA lifecycle or cleanup failed".into());}
        println!("{}",serde_json::json!({"started":started,"status":status,"stopped":stopped,"cleanup":{"socketRemoved":true,"pidFileRemoved":true,"settingsRemoved":true}}));
        return Ok(());
    }
    let args = args.strip_prefix(&["app-server".to_owned()]).unwrap_or(&args);
    let CliArgs::Server {listen,ws_auth,..} = parse_cli_args(args) else {return Err("Expected app-server mode arguments".into());};
    let cwd = std::env::current_dir()?.display().to_string();
    let agent_dir = maho_core::config::get_agent_dir();
    let runtime = AppServerRuntime::new(agent_dir,cwd,"1".into(),std::env::var("MAHO_SESSION_DIR").ok(),None).await;
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    run_app_server_mode(&runtime,listen,ws_auth,async move {
        tokio::select! {_ = terminate.recv() => {}, result = tokio::signal::ctrl_c() => {if let Err(error) = result {eprintln!("app-server signal: {error}");}}}
    }).await.map_err(Into::into)
}
