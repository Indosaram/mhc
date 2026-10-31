//! Where a host request proves endpoint keys from (senpi `rpc/host-request-key-root.ts`, senpi#3815): the caller's
//! explicit `--rpc-key-root`, then a gc's explicit target directory (it dials that directory's endpoints, whose
//! hosts published their records there), then an enclosing key root, `SENPI_RPC_KEY_ROOT`, and finally the
//! target's agent dir.
use crate::host_runner::HostRequest;
use std::path::{Path, PathBuf};

pub fn host_request_key_root(request: &HostRequest, explicit: Option<&Path>) -> PathBuf {
    if let Some(explicit) = explicit {
        return explicit.to_path_buf();
    }
    match request {
        HostRequest::Gc { agent_dir } => agent_dir.clone(),
        HostRequest::ShardPath { .. } => crate::host_probe::rpc_key_root(None, None),
        HostRequest::Ensure { target, .. }
        | HostRequest::Status { target, .. }
        | HostRequest::Stop { target, .. }
        | HostRequest::Handoff { target, .. } => {
            crate::host_probe::rpc_key_root(None, Some(&target.agent_dir))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host_runner::HostTarget;

    fn status(agent_dir: &str) -> HostRequest {
        HostRequest::Status {
            target: HostTarget {
                socket: "/t/rpc.sock".into(),
                agent_dir: agent_dir.into(),
            },
            include_workers: false,
            all: false,
        }
    }

    #[tokio::test]
    async fn explicit_root_wins_then_gc_target_then_enclosing_root_then_target_agent_dir() {
        let gc = HostRequest::Gc {
            agent_dir: "/gc-target".into(),
        };
        assert_eq!(
            host_request_key_root(&gc, Some(Path::new("/flag"))),
            Path::new("/flag")
        );
        // A gc's target directory is not overridden by an enclosing root.
        let enclosed = crate::host_probe::with_rpc_key_root("/caller".into(), async {
            (
                host_request_key_root(&gc, None),
                host_request_key_root(&status("/target"), None),
            )
        })
        .await;
        assert_eq!(enclosed.0, Path::new("/gc-target"));
        assert_eq!(enclosed.1, Path::new("/caller"));
        if std::env::var_os("SENPI_RPC_KEY_ROOT").is_none() {
            assert_eq!(
                host_request_key_root(&status("/target"), None),
                Path::new("/target")
            );
        }
    }
}
