#[derive(Debug, Default, PartialEq, Eq)]
pub struct TuiOptions { pub continue_session: bool }
pub fn parse_tui_args(argv: &[String]) -> Result<TuiOptions, String> {
    let mut options = TuiOptions::default();
    for arg in argv { match arg.as_str() { "--continue" | "-c" => options.continue_session = true, _ => return Err(format!("Unknown argument: {arg}")) } }
    Ok(options)
}
#[derive(Debug, PartialEq, Eq)]
pub struct ServerOptions { pub socket_path: String, pub sessions_root: String }
pub fn parse_server_args(argv: &[String]) -> Result<ServerOptions, String> {
    let [socket, sessions, ..] = argv else { return Err("Server requires <socketPath> <sessionsRoot>".to_owned()); };
    if socket.is_empty() || sessions.is_empty() { return Err("Server requires <socketPath> <sessionsRoot>".to_owned()); }
    Ok(ServerOptions { socket_path: socket.clone(), sessions_root: sessions.clone() })
}
#[derive(Debug, PartialEq, Eq)]
pub struct WorkerOptions { pub sessions_root: String, pub cwd: String, pub session_id: Option<String> }
pub fn parse_worker_args(argv: &[String]) -> Result<WorkerOptions, String> {
    let [sessions, cwd, rest @ ..] = argv else { return Err("Session worker requires <sessionsRoot> <cwd> [sessionId]".to_owned()); };
    if sessions.is_empty() || cwd.is_empty() { return Err("Session worker requires <sessionsRoot> <cwd> [sessionId]".to_owned()); }
    Ok(WorkerOptions { sessions_root: sessions.clone(), cwd: cwd.clone(), session_id: rest.first().cloned() })
}

pub fn run_tui_entry(options: TuiOptions) -> Result<(), String> {
    #[cfg(unix)]
    {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|error| error.to_string())?;
        runtime.block_on(super::presentation::run_tui(super::presentation::TuiOptions { cwd: None, continue_session: options.continue_session }))
    }
    #[cfg(not(unix))]
    {
        let _ = options;
        Err("The mini presentation requires the Unix socket transport".to_owned())
    }
}

#[cfg(unix)]
pub fn run_worker_entry(options: WorkerOptions) -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|error| error.to_string())?;
    runtime.block_on(super::worker::run_session_worker(super::worker::SessionWorkerOptions { sessions_root: options.sessions_root, cwd: options.cwd, session_id: options.session_id }))
}

#[cfg(not(unix))]
pub fn run_worker_entry(_options: WorkerOptions) -> Result<(), String> { Err("The mini session worker requires the Unix socket transport".to_owned()) }

#[cfg(unix)]
pub fn run_server_entry(options: ServerOptions, cwd: &str, spawn: super::server::SpawnWorker) -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|error| error.to_string())?;
    runtime.block_on(async {
        let shutdown = maho_ai::utils::abort::AbortController::new();
        let signal = shutdown.signal();
        let mut interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt()).map_err(|error| error.to_string())?;
        let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).map_err(|error| error.to_string())?;
        let server = super::server::run_server(std::path::Path::new(&options.socket_path), &options.sessions_root, cwd, spawn, &signal, None);
        tokio::pin!(server);
        tokio::select! { result = &mut server => result, _ = interrupt.recv() => { shutdown.abort(None); server.await }, _ = terminate.recv() => { shutdown.abort(None); server.await } }
    })
}

#[cfg(not(unix))]
pub fn run_server_entry(_options: ServerOptions, _cwd: &str, _spawn: super::server::SpawnWorker) -> Result<(), String> { Err("The mini session server requires the Unix socket transport".to_owned()) }
