//! Port of senpi `experimental/mini/tui/run.ts`.
//!
//! The presentation host: find or start the session server, attach to a session, run the view.

use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::collections::BTreeMap;
#[cfg(unix)]
use std::time::Duration;

use super::shared::protocol::SessionSummary;
#[cfg(unix)]
use super::shared::transport::SocketTransport;
#[cfg(unix)]
use super::session::{connect, list_sessions};
#[cfg(unix)]
use super::view::run_view;

#[cfg(unix)]
const SERVER_START_TIMEOUT: Duration = Duration::from_secs(10);
#[cfg(unix)]
const SERVER_CONNECT_RETRY: Duration = Duration::from_millis(50);

pub struct PresentationPaths {
    pub root: PathBuf,
    pub socket: PathBuf,
    pub sessions_root: PathBuf,
}

pub fn presentation_paths(agent: &Path) -> PresentationPaths {
    let root = agent.join("experimental");
    PresentationPaths { socket: root.join("mini.sock"), sessions_root: root.join("mini-sessions"), root }
}

pub fn continued_session<'a>(sessions: &'a [SessionSummary], cwd: &str) -> Option<&'a str> {
    // Stable ascending sort followed by at(-1) chooses the last input on a timestamp tie.
    sessions.iter().filter(|session| session.cwd == cwd).fold(None::<&SessionSummary>, |latest, candidate| {
        if latest.is_none_or(|latest| candidate.created_at >= latest.created_at) { Some(candidate) } else { latest }
    }).map(|session| session.id.as_str())
}

#[cfg(unix)]
pub async fn ensure_server(transport: &SocketTransport, socket_path: &Path, sessions_root: &str, cwd: &Path, env: &BTreeMap<String, String>) -> Result<(), String> {
    if transport.connect().await.is_ok() {
        return Ok(());
    }
    let args = vec![socket_path.to_string_lossy().into_owned(), sessions_root.to_owned()];
    let mut child = crate::experimental::process::spawn_internal_process(crate::experimental::process::InternalProcessRole::Server, &args, cwd, env).map_err(|error| error.to_string())?;
    let deadline = tokio::time::Instant::now() + SERVER_START_TIMEOUT;
    loop {
        if transport.connect().await.is_ok() {
            return Ok(());
        }
        if child.try_wait().map_err(|error| error.to_string())?.is_some() {
            return Err("Mini session server exited during startup".to_owned());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err("Timed out waiting for the mini session server".to_owned());
        }
        tokio::time::sleep(SERVER_CONNECT_RETRY).await;
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct TuiOptions {
    pub cwd: Option<String>,
    pub continue_session: bool,
}

#[cfg(unix)]
pub async fn run_tui(options: TuiOptions) -> Result<(), String> {
    let cwd = match &options.cwd {
        Some(cwd) => cwd.clone(),
        None => std::env::current_dir().map_err(|error| error.to_string())?.to_string_lossy().into_owned(),
    };
    let agent = maho_core::config::get_agent_dir();
    let paths = presentation_paths(Path::new(&agent));
    tokio::fs::create_dir_all(&paths.root).await.map_err(|error| error.to_string())?;
    let transport = SocketTransport { path: paths.socket.clone() };
    let sessions_root = paths.sessions_root.to_string_lossy().into_owned();
    let env: BTreeMap<String, String> = std::env::vars().collect();
    ensure_server(&transport, &paths.socket, &sessions_root, Path::new(&cwd), &env).await?;
    let session_id = if options.continue_session {
        let sessions = list_sessions(&transport).await?;
        continued_session(&sessions, &cwd).map(str::to_owned)
    } else {
        None
    };
    let client = connect(&transport, session_id.as_deref(), &cwd).await?;
    let result = run_view(&client, &cwd).await;
    client.close();
    result
}
