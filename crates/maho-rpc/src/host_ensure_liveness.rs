//! What an ensure may conclude about a registered generation that did not answer (senpi
//! `rpc/host-ensure-liveness.ts`): whether anything still accepts at the public path, and whether something
//! stands behind a path no registration names. Neither check ever signals anything.
//!
//! senpi `refuseIfStalled` needs `hostLoopStalled` from `host-stalled-evidence.ts`, which the Rust
//! `host_stalled_evidence` module does not export yet; it is not ported here.
pub use crate::host_ensure::public_endpoint_accepts;
use crate::socket_ownership::{
    ListenerVerdict, lsof_socket_holders, socket_entry_listener_state, stat_socket_identity,
};
use std::path::Path;

fn node_platform() -> &'static str {
    match std::env::consts::OS {
        "windows" => "win32",
        "macos" => "darwin",
        other => other,
    }
}

/// Whether something stands behind a public path that no registration names: the gate before an ensure
/// would start a host there. Unlike [`public_endpoint_accepts`] it also counts a listener that refuses only
/// because its backlog is full, because a start there could only lose the bind. An entry this process
/// cannot stat is held.
pub async fn unregistered_endpoint_held(socket: &str) -> bool {
    if cfg!(windows) || socket.starts_with('\0') {
        return false;
    }
    match stat_socket_identity(Path::new(socket)) {
        Ok(None) => false,
        Err(_) => true,
        Ok(Some(_)) => {
            socket_entry_listener_state(socket, node_platform(), lsof_socket_holders)
                .await
                .verdict
                == ListenerVerdict::Live
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn only_a_live_listener_holds_an_unregistered_path() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("socket");
        let text = path.to_string_lossy().into_owned();
        assert!(!unregistered_endpoint_held(&text).await);
        let listener = std::os::unix::net::UnixListener::bind(&path).expect("bind");
        assert!(unregistered_endpoint_held(&text).await);
        drop(listener);
        if cfg!(target_os = "linux") {
            assert!(
                !unregistered_endpoint_held(&text).await,
                "a refused entry on linux is dead"
            );
        }
        assert!(!unregistered_endpoint_held("\0abstract").await);
    }
}
