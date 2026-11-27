//! Thread host socket path resolution (`thread/live-surface-paths.ts`).

use std::path::{Path, PathBuf};

pub const THREAD_SOCKET_ENV_NAMES: &[&str] = &[
    "SENPI_RPC_SOCKET",
    "PI_RPC_SOCKET",
    "MHC_RPC_SOCKET",
    "OMO_RPC_SOCKET_PATH",
];

pub fn resolve_thread_socket(agent_dir: &Path) -> String {
    for name in THREAD_SOCKET_ENV_NAMES {
        if let Ok(val) = std::env::var(name)
            && !val.trim().is_empty()
        {
            return val.trim().to_owned();
        }
    }
    maho_rpc::rpc_client::default_rpc_socket_path(agent_dir)
        .to_string_lossy()
        .into_owned()
}

pub fn canonical_socket(socket: &str) -> PathBuf {
    let p = Path::new(socket);
    match (p.parent(), p.file_name()) {
        (Some(parent), Some(name)) => std::fs::canonicalize(parent)
            .map(|real| real.join(name))
            .unwrap_or_else(|_| p.to_path_buf()),
        _ => p.to_path_buf(),
    }
}

pub fn is_thread_socket(host_socket: &str, agent_dir: &Path) -> bool {
    canonical_socket(host_socket) == canonical_socket(&resolve_thread_socket(agent_dir))
}
