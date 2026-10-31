//! What an ensure may conclude about a registered generation that did not answer (senpi
//! `rpc/host-ensure-liveness.ts`): whether anything still accepts at the public path, and whether something
//! stands behind a path no registration names, and whether the generation is alive but stalled (senpi#2566,
//! IS-5). Neither check ever signals anything.
use crate::host_decision::{HostEnsureRefusedError, HostProtocolInfo, HostRefusalReason};
pub use crate::host_ensure::{public_endpoint_accepts, read_host_holder, refuse_if_foreign_held};
use crate::socket_ownership::{
    ListenerVerdict, lsof_socket_holders, socket_entry_listener_state, stat_socket_identity,
};
use std::path::Path;

/// IS-5: a generation nothing can reach whose loop is measurably stalled is still serving its sessions, so it
/// is neither stopped nor stranded beside a new generation: no signal, no successor, registration intact.
pub fn refuse_if_stalled(
    paths: &crate::host_daemon_paths::HostDaemonPaths,
    instance_id: &str,
    socket: &str,
    protocol: Option<&HostProtocolInfo>,
) -> Result<(), HostEnsureRefusedError> {
    let env: std::collections::HashMap<String, String> = std::env::vars().collect();
    let generation = crate::host_daemon_paths::generation_paths(paths, instance_id);
    let stalled = crate::host_stalled_evidence::host_loop_stalled(
        &generation,
        crate::host_stalled_evidence::LoopStallWindow {
            now: crate::host_stalled_evidence::now_ms(),
            error_ms: loop_lag_error_ms(&env) as i64,
            window_ms: crate::host_stalled_evidence::stall_refusal_window_ms(&env) as i64,
        },
    );
    if stalled && crate::host_stalled_evidence::host_child_alive(&generation, |_| {}) != Some(false)
    {
        return Err(HostEnsureRefusedError::new(
            socket,
            HostRefusalReason::HostStalled,
            protocol,
        ));
    }
    Ok(())
}

/// The drift past which a tick is a stall, as the host itself judges it (senpi `loopLagErrorMs`).
fn loop_lag_error_ms(env: &std::collections::HashMap<String, String>) -> f64 {
    crate::host_lifecycle::parse_idle_exit_ms(
        env.get(crate::loop_lag_watchdog::LOOP_LAG_ERROR_MS_ENV)
            .map(String::as_str),
    )
    .unwrap_or(crate::loop_lag_watchdog::DEFAULT_LOOP_LAG_ERROR_MS)
}

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

    #[test]
    fn a_stalled_generation_with_a_live_or_unrecorded_child_is_refused() {
        let temp = tempfile::tempdir().expect("tempdir");
        let paths = crate::host_daemon_paths::host_daemon_directory_paths(temp.path());
        let generation = crate::host_daemon_paths::generation_paths(&paths, "g");
        std::fs::create_dir_all(&generation.dir).expect("generation dir");
        assert!(
            refuse_if_stalled(&paths, "g", "/s", None).is_ok(),
            "no evidence"
        );
        let at = chrono::Utc::now() - chrono::Duration::seconds(1);
        crate::host_stalled_evidence::write_stall_evidence(
            &generation.dir,
            &crate::host_stalled_evidence::HostStallEvidence {
                at: at.to_rfc3339(),
                drift_ms: 9_000.,
                process_cpu_ms: 0.,
                heap_delta_mb: 0.,
                session_id: None,
                tool: None,
            },
        )
        .expect("evidence");
        let refused = refuse_if_stalled(&paths, "g", "/s", None).expect_err("stalled");
        assert_eq!(refused.reason, HostRefusalReason::HostStalled);
        let mut dead = std::process::Command::new("true").spawn().expect("spawn");
        let pid = dead.id();
        dead.wait().expect("wait");
        std::fs::write(
            &generation.child_pid_file,
            serde_json::json!({"pid": pid}).to_string(),
        )
        .expect("child pid");
        assert!(
            refuse_if_stalled(&paths, "g", "/s", None).is_ok(),
            "a dead child is not a stalled host"
        );
    }
}
