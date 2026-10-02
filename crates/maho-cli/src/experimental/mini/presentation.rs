use std::path::{Path, PathBuf};
use super::shared::protocol::SessionSummary;
pub struct PresentationPaths { pub root: PathBuf, pub socket: PathBuf, pub sessions_root: PathBuf }
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
