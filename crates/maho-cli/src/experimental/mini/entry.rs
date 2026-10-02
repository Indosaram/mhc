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
