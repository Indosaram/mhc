//! Port of senpi packages/coding-agent/src/modes/interactive/session-control-registry.ts (v2026.10.27-2).
//!
//! Where a terminal's control socket lives and how it appears in the one endpoint registry
//! (`<agentDir>/rpc-host-daemon/<16hex>/`, kind `tui`).
//!
//! Registration runs under the endpoint's ensure lock (the lock `host gc` takes) and writes the
//! generation record BEFORE `endpoint.json`: a reader that can see the endpoint can always see a
//! live generation behind it, so the endpoint is never judged `dead` while it is being registered.
//! The directory is named by a socket built from a fresh instance id, so it never holds an earlier
//! generation. A TUI endpoint has nothing to reattach to, so a clean exit removes its whole directory.
use maho_rpc::host_daemon_paths::{
    HostDaemonPaths, create_daemon_directories_as, create_host_daemon_paths,
};
use maho_rpc::host_endpoints::EndpointKind;
use maho_rpc::socket_ownership::MAX_SOCKET_PATH_BYTES;
use maho_rpc::socket_transport::socket_secret_path;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

#[cfg(unix)]
const PRIVATE_DIRECTORY_MODE: u32 = 0o700;
const REGISTRY_LOCK_WAIT_MS: u64 = 5_000;
/// senpi `process.platform` for the lock address.
const PLATFORM: &str = if cfg!(windows) { "win32" } else { "linux" };

/// `<agentDir>/rpc/tui/t-<16hex>.sock`, or - when the agent directory is too deep for `sun_path` -
/// the same name under `/tmp/senpi-rpc-<sha256(agentDir)[:8]>/tui/`. The fallback root is
/// predictable, so it is used only when it is a real directory owned by this user with no group or
/// other access; anything else fails the registration instead of binding into it. On win32 the path
/// is only the logical address (the listener binds a named pipe hashed from it), so it never moves
/// (senpi#4267).
pub fn resolve_tui_socket(agent_dir: &str, instance_id: &str) -> Result<String, String> {
    let name = maho_rpc::tui_socket::tui_socket_name(instance_id);
    let primary = Path::new(agent_dir).join("rpc").join("tui").join(&name);
    let primary_text = primary.to_string_lossy().into_owned();
    if cfg!(windows) || primary_text.len() <= MAX_SOCKET_PATH_BYTES {
        if let Some(parent) = primary.parent() {
            make_private_directory(parent)?;
        }
        return Ok(primary_text);
    }
    let digest = Sha256::digest(agent_dir.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let fallback_name = format!("senpi-rpc-{}", digest.get(..8).unwrap_or(&digest));
    let scoped_root = std::env::temp_dir().join(&fallback_name);
    // Test launchers supply a short owned TMPDIR. A normal user's long macOS temp path still needs
    // the original short system fallback to fit sun_path.
    let root =
        if scoped_root.join("tui").join(&name).to_string_lossy().len() <= MAX_SOCKET_PATH_BYTES {
            scoped_root
        } else {
            PathBuf::from("/tmp").join(&fallback_name)
        };
    let tui_root = root.join("tui");
    make_private_directory(&tui_root)?;
    for directory in [&root, &tui_root] {
        assert_private_directory(directory)?;
    }
    let fallback = tui_root.join(&name).to_string_lossy().into_owned();
    if fallback.len() > MAX_SOCKET_PATH_BYTES {
        return Err(format!(
            "socket path {fallback} exceeds {MAX_SOCKET_PATH_BYTES} bytes"
        ));
    }
    Ok(fallback)
}

fn make_private_directory(directory: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(PRIVATE_DIRECTORY_MODE)
            .create(directory)
            .map_err(|error| error.to_string())?;
        std::fs::set_permissions(
            directory,
            std::fs::Permissions::from_mode(PRIVATE_DIRECTORY_MODE),
        )
        .map_err(|error| error.to_string())
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(directory).map_err(|error| error.to_string())
    }
}

fn assert_private_directory(directory: &Path) -> Result<(), String> {
    let unsafe_root = || {
        format!(
            "alt_root_unsafe: {} is not a private directory of this user",
            directory.display()
        )
    };
    let real = std::fs::canonicalize(directory).map_err(|error| error.to_string())?;
    let stat = std::fs::symlink_metadata(real).map_err(|error| error.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let owned_by_us = stat.uid() == rustix::process::getuid().as_raw();
        if !stat.is_dir() || !owned_by_us || stat.mode() & 0o077 != 0 {
            return Err(unsafe_root());
        }
    }
    #[cfg(not(unix))]
    if !stat.is_dir() {
        return Err(unsafe_root());
    }
    Ok(())
}

#[derive(Clone, Debug)]
pub struct TuiRegistryEntry {
    pub paths: HostDaemonPaths,
    pub socket: String,
}

pub async fn register_tui_endpoint(
    agent_dir: &str,
    socket: &str,
    instance_id: &str,
) -> Result<TuiRegistryEntry, String> {
    let paths = create_host_daemon_paths(socket, Path::new(agent_dir));
    let mut lock = maho_rpc::host_ensure_lock::acquire_host_ensure_lock(
        socket,
        REGISTRY_LOCK_WAIT_MS,
        PLATFORM,
    )
    .await
    .map_err(|error| error.to_string())?;
    let written = (|| {
        maho_rpc::host_daemon_registration::write_host_registration(
            &paths,
            &maho_rpc::host_daemon_registration::HostRegistration {
                pid: std::process::id(),
                process_start_time: maho_rpc::host_daemon_registration::this_process_start_time(),
                socket: socket.to_owned(),
                instance_id: instance_id.to_owned(),
                generation: 0.0,
                launch_profile_id: "tui".into(),
                build: Some(maho_rpc::host_daemon_registration::RegisteredBuild::this_engine()),
            },
        )
        .map_err(|error| error.to_string())?;
        create_daemon_directories_as(&paths, Some(EndpointKind::Tui))
            .map_err(|error| error.to_string())
    })();
    let released = lock.release().map_err(|error| error.to_string());
    written?;
    released?;
    Ok(TuiRegistryEntry {
        paths,
        socket: socket.to_owned(),
    })
}

pub async fn unregister_tui_endpoint(entry: &TuiRegistryEntry) {
    let lock = maho_rpc::host_ensure_lock::acquire_host_ensure_lock(
        &entry.socket,
        REGISTRY_LOCK_WAIT_MS,
        PLATFORM,
    )
    .await
    .ok();
    unregister_tui_endpoint_sync(entry);
    if let Some(mut lock) = lock {
        drop(lock.release());
    }
}

pub fn unregister_tui_endpoint_sync(entry: &TuiRegistryEntry) {
    remove_file_if_present(Path::new(&entry.socket));
    if let Ok(secret) = socket_secret_path(&entry.socket) {
        remove_file_if_present(Path::new(&secret));
    }
    drop(std::fs::remove_dir_all(&entry.paths.dir));
}

fn remove_file_if_present(path: &Path) {
    drop(std::fs::remove_file(path));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_short_agent_dir_keeps_the_socket_under_rpc_tui_with_a_private_directory() {
        let directory = tempfile::tempdir().expect("directory");
        let agent_dir = directory.path().to_string_lossy().into_owned();
        let socket = resolve_tui_socket(&agent_dir, "instance").expect("socket");
        let expected = Path::new(&agent_dir)
            .join("rpc")
            .join("tui")
            .join(maho_rpc::tui_socket::tui_socket_name("instance"));
        assert_eq!(socket, expected.to_string_lossy());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(expected.parent().expect("parent"))
                .expect("stat")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o700);
        }
    }
}
