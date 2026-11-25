//! Read-only daemon state view for host-session end confirmation (`runners/rpc-host/generation-view.ts`).

use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use sha2::{Digest, Sha256};

use crate::lifecycle::host_session::HostGenerationView;
use crate::state::HostSessionIdentity;

fn is_valid_generation_id(id: &str) -> bool {
    if id.is_empty() || id.len() > 128 || id.contains("..") {
        return false;
    }
    let mut bytes = id.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    first.is_ascii_alphanumeric()
        && bytes.all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

fn sha256_prefix_16(input: &str) -> String {
    format!("{:x}", Sha256::digest(input.as_bytes()))[..16].to_string()
}

/// Equivalent to `hostDaemonDir(agentDir, socket)` matching `maho_rpc::host_daemon_paths`.
pub fn host_daemon_dir(agent_dir: &Path, socket: &str) -> PathBuf {
    agent_dir
        .join("rpc-host-daemon")
        .join(sha256_prefix_16(socket))
}

fn present(path: &Path) -> bool {
    match std::fs::metadata(path) {
        Ok(_) => true,
        Err(error) => error.kind() != ErrorKind::NotFound,
    }
}

/// The host keys a parked entry by the path it opened the session with, which may be canonical.
fn session_path_spellings(host_session: &HostSessionIdentity) -> Vec<String> {
    match std::fs::canonicalize(&host_session.session_path) {
        Ok(canonical) => {
            let canonical_str = canonical.to_string_lossy().into_owned();
            if canonical_str == host_session.session_path {
                vec![canonical_str]
            } else {
                vec![host_session.session_path.clone(), canonical_str]
            }
        }
        Err(_) => vec![host_session.session_path.clone()],
    }
}

struct FsHostGenerationView {
    agent_dir: PathBuf,
}

impl HostGenerationView for FsHostGenerationView {
    fn generation_present(&self, host_session: &HostSessionIdentity) -> bool {
        if !is_valid_generation_id(&host_session.instance_id) {
            return true;
        }
        let path = host_daemon_dir(&self.agent_dir, &host_session.socket)
            .join("generations")
            .join(&host_session.instance_id);
        present(&path)
    }

    fn parked_entry_present(&self, host_session: &HostSessionIdentity) -> bool {
        let dir = host_daemon_dir(&self.agent_dir, &host_session.socket).join("handover-parked");
        for session_path in session_path_spellings(host_session) {
            let file = dir.join(format!("{}.json", sha256_prefix_16(&session_path)));
            if present(&file) {
                return true;
            }
        }
        false
    }
}

/// Read-only view of `<agentDir>/rpc-host-daemon/<endpoint>/`. Anything other than `NotFound` reads
/// as `true` so unreadable state never proves a session ended (`#11765`).
pub fn create_host_generation_view(agent_dir: impl AsRef<Path>) -> Arc<dyn HostGenerationView> {
    Arc::new(FsHostGenerationView {
        agent_dir: agent_dir.as_ref().to_path_buf(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(socket: &str, instance_id: &str, session_path: &str) -> HostSessionIdentity {
        HostSessionIdentity {
            socket: socket.to_string(),
            instance_id: instance_id.to_string(),
            session_path: session_path.to_string(),
            routing_id: "r1".to_string(),
            daemon_pid: None,
            effect_seq: None,
            adopted_entry_id: None,
        }
    }

    #[test]
    fn generation_and_parked_checks_read_daemon_layout_and_reject_bad_ids() {
        let dir = tempfile::tempdir().expect("tempdir");
        let view = create_host_generation_view(dir.path());
        let socket = "/tmp/test.sock";
        let daemon = host_daemon_dir(dir.path(), socket);
        let session_file = dir.path().join("child.jsonl");
        std::fs::write(&session_file, "{}").expect("session file");

        let id = identity(socket, "gen-1", &session_file.to_string_lossy());
        assert!(!view.generation_present(&id));
        assert!(!view.parked_entry_present(&id));

        std::fs::create_dir_all(daemon.join("generations").join("gen-1")).expect("gen dir");
        assert!(view.generation_present(&id));

        std::fs::create_dir_all(daemon.join("handover-parked")).expect("parked dir");
        let canonical = std::fs::canonicalize(&session_file)
            .expect("canon")
            .to_string_lossy()
            .into_owned();
        std::fs::write(
            daemon
                .join("handover-parked")
                .join(format!("{}.json", sha256_prefix_16(&canonical))),
            "{}",
        )
        .expect("parked file");
        assert!(view.parked_entry_present(&id));

        // Traversal / invalid generation ids conservatively return true.
        assert!(view.generation_present(&identity(socket, "../escape", "x")));
        assert!(view.generation_present(&identity(socket, "", "x")));
    }
}
