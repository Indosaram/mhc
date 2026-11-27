//! The registry record of a socket host that binds its own public socket (`--listen`, `--headless`).
//!
//! Writes `endpoint.json` itself before serving, and binds inside the endpoint's ensure lock (the
//! lock `host gc` judges and removes under). A supervised host never comes here - its ensure wrote
//! the record and holds that lock while it starts the host.

use crate::host_daemon_paths::{create_daemon_directories, create_host_daemon_paths};
use crate::host_ensure_lock::acquire_host_ensure_lock;
use std::future::Future;
use std::path::PathBuf;

/// Longer than a gc's hold of one endpoint (its 2 s lock wait, the evidence probes, the removal).
const REGISTRATION_LOCK_WAIT_MS: u64 = 10_000;

pub struct BareEndpointRegistration<L>
where
    L: FnMut(String),
{
    pub agent_dir: PathBuf,
    pub socket: String,
    pub log: L,
}

pub async fn bind_registered_endpoint<T, F, L>(
    mut registration: BareEndpointRegistration<L>,
    bind: impl FnOnce() -> F,
) -> T
where
    F: Future<Output = T>,
    L: FnMut(String),
{
    // An abstract socket has no path, so it has no daemon directory either.
    if registration.socket.starts_with('\0') {
        return bind().await;
    }
    let platform = if cfg!(windows) {
        "win32"
    } else {
        std::env::consts::OS
    };
    let mut lock =
        match acquire_host_ensure_lock(&registration.socket, REGISTRATION_LOCK_WAIT_MS, platform)
            .await
        {
            Ok(lock) => Some(lock),
            Err(cause) => {
                (registration.log)(format!(
                    "endpoint ensure lock unavailable, registering without it: {cause}"
                ));
                None
            }
        };
    let paths = create_host_daemon_paths(&registration.socket, &registration.agent_dir);
    if let Err(cause) = create_daemon_directories(&paths) {
        (registration.log)(format!("endpoint registry record not written: {cause}"));
    }
    let result = bind().await;
    if let Some(lock) = lock.as_mut() {
        let _ = lock.release();
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn registers_endpoint_directory_around_bind_and_skips_abstract_socket() {
        let temp = tempfile::tempdir().expect("tempdir");
        let socket = temp
            .path()
            .join("rpc/bare.sock")
            .to_string_lossy()
            .into_owned();
        let mut logs = Vec::new();
        let out = bind_registered_endpoint(
            BareEndpointRegistration {
                agent_dir: temp.path().to_path_buf(),
                socket: socket.clone(),
                log: |msg| logs.push(msg),
            },
            || async { 42u32 },
        )
        .await;
        assert_eq!(out, 42);
        assert!(logs.is_empty());
        let paths = create_host_daemon_paths(&socket, temp.path());
        assert!(paths.endpoint_file.exists());

        let abstract_out = bind_registered_endpoint(
            BareEndpointRegistration {
                agent_dir: temp.path().to_path_buf(),
                socket: "\0abstract-bare".into(),
                log: |_| {},
            },
            || async { "ok" },
        )
        .await;
        assert_eq!(abstract_out, "ok");
    }
}
