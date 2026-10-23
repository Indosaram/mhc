use sha2::{Digest, Sha256};
use std::{
    fs, io,
    path::{Path, PathBuf},
};
pub const HOST_DAEMON_LAYOUT: u32 = 2;
pub const HOST_DAEMON_DIR_ENV: &str = "SENPI_RPC_HOST_DAEMON_DIR";
pub const HOST_STATE_FILE_MODE: u32 = 0o600;
#[derive(Debug, Clone)]
pub struct HostDaemonPaths {
    pub flat_dir: PathBuf,
    pub layout_marker: PathBuf,
    pub legacy_pid_file: PathBuf,
    pub dir: PathBuf,
    pub pointer_file: PathBuf,
    pub lock_file: PathBuf,
    pub settings_file: PathBuf,
    pub stderr_log: PathBuf,
    pub generations_dir: PathBuf,
    pub reservations_dir: PathBuf,
    pub endpoint_file: PathBuf,
}
#[derive(Debug, Clone)]
pub struct HostGenerationPaths {
    pub dir: PathBuf,
    pub relative_dir: String,
    pub pid_file: PathBuf,
    pub settings_file: PathBuf,
    pub scratch_dir: PathBuf,
    /// The host CHILD's `{pid, processStartTime}`, readable apart from the supervisor's.
    pub child_pid_file: PathBuf,
    /// Written by whoever is about to signal this generation, before the signal.
    pub stop_intent_file: PathBuf,
    /// Written by whoever asks this generation to drain; read by its supervisor (signal-free drain).
    pub drain_request_file: PathBuf,
    /// The supervisor's relay of a drain to its host child.
    pub child_drain_request_file: PathBuf,
    /// The supervisor's marker that it reads drain requests, naming its pid.
    pub drain_watch_file: PathBuf,
    /// The newest stall the host's loop-lag watchdog measured.
    pub stalled_file: PathBuf,
    /// The host loop's heartbeat, refreshed on every healthy watchdog tick.
    pub alive_file: PathBuf,
    /// The supervisor's report that it is waiting out a stalled child before escalating.
    pub stop_progress_file: PathBuf,
}
#[derive(Debug, thiserror::Error)]
#[error("RPC daemon state directory {path} is not usable: {source}")]
pub struct HostDaemonStateError {
    pub path: PathBuf,
    #[source]
    pub source: io::Error,
}
pub fn daemon_directory_name(socket: &str) -> String {
    format!("{:x}", Sha256::digest(socket.as_bytes()))[..16].into()
}
pub fn create_host_daemon_paths(socket: &str, agent_dir: &Path) -> HostDaemonPaths {
    host_daemon_directory_paths(
        &agent_dir
            .join("rpc-host-daemon")
            .join(daemon_directory_name(socket)),
    )
}
pub fn host_daemon_directory_paths(dir: &Path) -> HostDaemonPaths {
    let flat_dir = dir.parent().unwrap_or(Path::new("")).to_path_buf();
    HostDaemonPaths {
        layout_marker: flat_dir.join("layout.json"),
        legacy_pid_file: flat_dir.join("host.pid"),
        flat_dir,
        dir: dir.into(),
        pointer_file: dir.join("host.pid"),
        lock_file: dir.join("daemon.lock"),
        settings_file: dir.join("settings.json"),
        stderr_log: dir.join("stderr.log"),
        generations_dir: dir.join("generations"),
        reservations_dir: dir.join("reservations"),
        endpoint_file: dir.join("endpoint.json"),
    }
}
pub fn generation_paths(paths: &HostDaemonPaths, instance_id: &str) -> HostGenerationPaths {
    let dir = paths.generations_dir.join(instance_id);
    HostGenerationPaths {
        relative_dir: format!("generations/{instance_id}"),
        pid_file: dir.join("host.pid"),
        settings_file: dir.join("settings.json"),
        scratch_dir: dir.join("scratch"),
        child_pid_file: dir.join("host-child.pid"),
        stop_intent_file: dir.join("stop-intent.json"),
        drain_request_file: dir.join("drain-request.json"),
        child_drain_request_file: dir.join("host-drain-request.json"),
        drain_watch_file: dir.join("drain-watch.json"),
        stalled_file: dir.join("host-stalled.json"),
        alive_file: dir.join("host-alive.json"),
        stop_progress_file: dir.join("stop-progress.json"),
        dir,
    }
}
pub fn create_private_directory(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(path)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
    }
    #[cfg(not(unix))]
    {
        fs::create_dir_all(path)
    }
}
pub fn create_daemon_directories(paths: &HostDaemonPaths) -> Result<(), HostDaemonStateError> {
    use std::io::Write;
    let result = (|| -> io::Result<()> {
        #[cfg(unix)]
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
        #[cfg(unix)]
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&paths.flat_dir)?;
        #[cfg(not(unix))]
        fs::create_dir_all(&paths.flat_dir)?;
        for directory in [&paths.dir, &paths.generations_dir, &paths.reservations_dir] {
            create_private_directory(directory)?;
        }
        let mut options = fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        options.mode(HOST_STATE_FILE_MODE);
        let mut file = options.open(&paths.layout_marker)?;
        let name = paths.dir.file_name().unwrap_or_default().to_string_lossy();
        writeln!(
            file,
            "{}",
            serde_json::json!({"layout":HOST_DAEMON_LAYOUT,"dir":name})
        )
    })();
    result.map_err(|source| HostDaemonStateError {
        path: paths.dir.clone(),
        source,
    })
}
pub fn create_generation_directory(
    generation: &HostGenerationPaths,
) -> Result<(), HostDaemonStateError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        let result = (|| {
            fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(&generation.scratch_dir)?;
            fs::set_permissions(&generation.dir, fs::Permissions::from_mode(0o700))
        })();
        result.map_err(|source| HostDaemonStateError {
            path: generation.dir.clone(),
            source,
        })
    }
    #[cfg(not(unix))]
    {
        fs::create_dir_all(&generation.scratch_dir).map_err(|source| HostDaemonStateError {
            path: generation.dir.clone(),
            source,
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hash_matches_sha256_prefix() {
        assert_eq!(daemon_directory_name("abc"), "ba7816bf8f01cfea");
        assert_ne!(daemon_directory_name("ABC"), daemon_directory_name("abc"));
    }
    #[test]
    fn endpoint_and_generation_paths() {
        let paths = create_host_daemon_paths("abc", Path::new("/tmp/agent"));
        assert_eq!(
            paths.dir,
            Path::new("/tmp/agent/rpc-host-daemon/ba7816bf8f01cfea")
        );
        let generation = generation_paths(&paths, "id");
        assert_eq!(generation.relative_dir, "generations/id");
        assert_eq!(generation.pid_file, generation.dir.join("host.pid"));
        assert_eq!(
            generation.drain_request_file,
            generation.dir.join("drain-request.json")
        );
        assert_eq!(
            generation.child_drain_request_file,
            generation.dir.join("host-drain-request.json")
        );
        assert_eq!(
            generation.drain_watch_file,
            generation.dir.join("drain-watch.json")
        );
        assert_eq!(
            generation.alive_file,
            generation.dir.join("host-alive.json")
        );
    }
    #[cfg(unix)]
    #[test]
    fn legacy_registration_and_mode_are_preserved() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let paths = create_host_daemon_paths("abc", temp.path());
        fs::create_dir(&paths.flat_dir).unwrap();
        fs::set_permissions(&paths.flat_dir, fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(&paths.legacy_pid_file, "legacy").unwrap();
        create_daemon_directories(&paths).unwrap();
        assert_eq!(fs::read_to_string(paths.legacy_pid_file).unwrap(), "legacy");
        assert_eq!(
            fs::metadata(paths.flat_dir).unwrap().permissions().mode() & 0o777,
            0o755
        );
        assert_eq!(
            fs::metadata(paths.dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(paths.layout_marker)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}
