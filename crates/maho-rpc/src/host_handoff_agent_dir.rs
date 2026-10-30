//! Whose host this is: a handoff never crosses agent directories (senpi `host-handoff-agent-dir.ts`, #3560).
//!
//! A client reads daemon state under ITS OWN agent dir. Pointed at another agent dir's socket it finds no
//! registration there, and the unregistered-host path would replace the idle host, handing every client of the
//! real agent dir to a host that reads and writes another directory. So the running host's agent dir is settled
//! first, and only a client of that same directory may replace it:
//!
//! - a host that advertises `agentDir` in `get_protocol_info` is taken at its word;
//! - an older host that does not is placed by the layout: when the socket is `<dir>/rpc/rpc.sock` and a layout-2
//!   registration under `<dir>` proves a live owner of it, the host belongs to `<dir>`;
//! - with neither, nothing is known and the handoff proceeds exactly as before.
//!
//! Both sides are compared canonically (realpath), so a symlinked spelling of the same directory is the same
//! directory. Nothing here signals or touches the running host.
use crate::host_protocol_info::HostProtocolInfo;
use std::path::{Path, PathBuf};

pub const FOREIGN_AGENT_DIR: &str = "foreign_agent_dir";

/// The refusal detail for a caller of another agent dir, or `None` when the host is the caller's (or unknown).
pub fn foreign_agent_dir_refusal(
    socket: &str,
    caller_agent_dir: &Path,
    host: &HostProtocolInfo,
) -> Option<String> {
    let host_agent_dir = host
        .agent_dir
        .as_deref()
        .map(PathBuf::from)
        .or_else(|| layout_agent_dir(socket))?;
    agent_dir_mismatch(socket, caller_agent_dir, &host_agent_dir)
}

/// Why a client of `caller_agent_dir` may not replace the host of `host_agent_dir`, or `None` when both are one
/// directory.
pub fn agent_dir_mismatch(
    socket: &str,
    caller_agent_dir: &Path,
    host_agent_dir: &Path,
) -> Option<String> {
    let ours = canonical_directory(caller_agent_dir);
    let theirs = canonical_directory(host_agent_dir);
    if ours == theirs {
        return None;
    }
    Some(format!(
        "{socket} is served by the host of agent dir {}, and this client's agent dir is {}; a host is only replaced by a client of its own agent dir - run with that agent dir to upgrade it, or use a socket of your own",
        theirs.display(),
        ours.display()
    ))
}

/// The agent dir an older host belongs to, when its socket is that dir's default and a registration there proves it.
fn layout_agent_dir(socket: &str) -> Option<PathBuf> {
    let socket_path = Path::new(socket);
    let rpc_directory = socket_path.parent()?;
    if socket_path.file_name()? != "rpc.sock" || rpc_directory.file_name()? != "rpc" {
        return None;
    }
    let agent_dir = rpc_directory.parent()?;
    let paths = crate::host_daemon_paths::create_host_daemon_paths(socket, agent_dir);
    let registered = crate::host_daemon_registration::read_host_registration(&paths)
        .ok()
        .flatten();
    crate::host_daemon_registration::proven_owner(registered.as_ref(), socket)?;
    Some(agent_dir.to_path_buf())
}

/// A directory that does not exist (yet) has no realpath; its resolved spelling is the best name for it.
fn canonical_directory(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| std::path::absolute(path).unwrap_or(path.into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn host(agent_dir: Option<&str>) -> HostProtocolInfo {
        let mut host = crate::host_protocol_info::parse_host_protocol_info(
            &serde_json::json!({"serverVersion":"1","capabilities":[]}),
        )
        .unwrap();
        host.agent_dir = agent_dir.map(str::to_owned);
        host
    }
    #[test]
    fn an_advertised_other_dir_is_refused_and_the_same_dir_is_not() {
        let temp = tempfile::tempdir().unwrap();
        let ours = temp.path().join("ours");
        let theirs = temp.path().join("theirs");
        std::fs::create_dir_all(&ours).unwrap();
        std::fs::create_dir_all(&theirs).unwrap();
        let detail = foreign_agent_dir_refusal("/s.sock", &ours, &host(theirs.to_str())).unwrap();
        assert!(detail.contains("only replaced by a client of its own agent dir"));
        assert!(foreign_agent_dir_refusal("/s.sock", &ours, &host(ours.to_str())).is_none());
    }
    #[cfg(unix)]
    #[test]
    fn a_symlinked_spelling_is_the_same_directory() {
        let temp = tempfile::tempdir().unwrap();
        let real = temp.path().join("real");
        std::fs::create_dir_all(&real).unwrap();
        let link = temp.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert!(agent_dir_mismatch("/s.sock", &link, &real).is_none());
    }
    #[test]
    fn an_older_host_without_a_layout_proof_is_unknown() {
        let temp = tempfile::tempdir().unwrap();
        let socket = temp.path().join("other/rpc/rpc.sock");
        assert!(
            foreign_agent_dir_refusal(socket.to_str().unwrap(), temp.path(), &host(None)).is_none()
        );
    }
}
