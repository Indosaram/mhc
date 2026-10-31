use sha2::{Digest, Sha256};
use std::{
    fs, io,
    path::{Path, PathBuf},
};
pub const HOST_DAEMON_LAYOUT: u32 = 2;
pub const HOST_DAEMON_DIR_ENV: &str = "SENPI_RPC_HOST_DAEMON_DIR";
pub const HOST_STATE_FILE_MODE: u32 = 0o600;
/// Version of the `endpoint.json` registry record.
pub const ENDPOINT_REGISTRY_VERSION: u32 = 1;
/// senpi `HostDaemonDirectory`: the paths inside one endpoint directory. The Rust record carries the flat
/// fields too; a directory-only caller ignores them.
pub type HostDaemonDirectory = HostDaemonPaths;
#[derive(Debug, Clone)]
pub struct HostDaemonPaths {
    /// The endpoint these paths belong to, exactly as its directory name was derived from; `None` for a reader
    /// that was told the directory but not its socket (senpi `HostDaemonDirectory`).
    pub socket: Option<String>,
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
    HostDaemonPaths {
        socket: Some(socket.to_owned()),
        ..host_daemon_directory_paths(
            &agent_dir
                .join("rpc-host-daemon")
                .join(daemon_directory_name(socket)),
        )
    }
}
pub fn host_daemon_directory_paths(dir: &Path) -> HostDaemonPaths {
    let flat_dir = dir.parent().unwrap_or(Path::new("")).to_path_buf();
    HostDaemonPaths {
        socket: None,
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
/// Creates this endpoint's directories, publishes the flat marker and writes `endpoint.json` for an `rpc_host`
/// (senpi `createDaemonDirectories`). A path set that was not derived from a socket has no identity to write.
pub fn create_daemon_directories(paths: &HostDaemonPaths) -> Result<(), HostDaemonStateError> {
    create_daemon_directories_as(paths, None)
}
/// [`create_daemon_directories`] for a registrant that names its kind: a `tui` control endpoint passes
/// [`EndpointKind::Tui`](crate::host_endpoints::EndpointKind::Tui); everything else is an `rpc_host`.
pub fn create_daemon_directories_as(
    paths: &HostDaemonPaths,
    kind: Option<crate::host_endpoints::EndpointKind>,
) -> Result<(), HostDaemonStateError> {
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
    })?;
    match &paths.socket {
        Some(socket) => ensure_endpoint_identity(paths, socket, false, kind),
        None => Ok(()),
    }
}
/// Writes `endpoint.json` when it is absent and never rewrites a valid one: the first writer's `created_at` is
/// the endpoint's birth (senpi `ensureEndpointIdentity`). The record is written whole to a temporary name and
/// LINKED into place, so a reader never sees it half-written and a racing second writer loses on the link.
/// `repair` - asserted only by an ensure under this socket's ensure lock - also replaces a file that does not
/// name a socket of this directory. A filesystem without hard links gets the exclusive create instead.
pub fn ensure_endpoint_identity(
    paths: &HostDaemonPaths,
    socket: &str,
    repair: bool,
    kind: Option<crate::host_endpoints::EndpointKind>,
) -> Result<(), HostDaemonStateError> {
    use std::io::Write;
    let temporary = paths.endpoint_file.with_file_name(format!(
        "endpoint.json.{}-{}.tmp",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let record = format!(
        "{}\n",
        serde_json::json!({
            "layout": HOST_DAEMON_LAYOUT,
            "registry_version": ENDPOINT_REGISTRY_VERSION,
            "endpoint_kind": kind.unwrap_or(crate::host_endpoints::EndpointKind::RpcHost).as_str(),
            "socket": socket,
            "created_at": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        })
    );
    let exclusive = |path: &Path| -> io::Result<bool> {
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, HOST_STATE_FILE_MODE);
        match options.open(path) {
            Ok(mut file) => file.write_all(record.as_bytes()).map(|()| true),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(false),
            Err(error) => Err(error),
        }
    };
    let result = (|| -> io::Result<()> {
        create_private_directory(&paths.dir)?;
        exclusive(&temporary)?;
        let placed = match fs::hard_link(&temporary, &paths.endpoint_file) {
            Ok(()) => true,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => false,
            Err(_) => exclusive(&paths.endpoint_file)?,
        };
        if !placed && repair && !names_this_directory(paths) {
            fs::rename(&temporary, &paths.endpoint_file)?;
        }
        Ok(())
    })();
    let _ = fs::remove_file(&temporary);
    result.map_err(|source| HostDaemonStateError {
        path: paths.endpoint_file.clone(),
        source,
    })
}
fn names_this_directory(paths: &HostDaemonPaths) -> bool {
    let Some(record) = fs::read_to_string(&paths.endpoint_file)
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
    else {
        return false;
    };
    let name = paths.dir.file_name().unwrap_or_default().to_string_lossy();
    let platform = if cfg!(windows) {
        "win32"
    } else {
        std::env::consts::OS
    };
    record
        .get("socket")
        .and_then(serde_json::Value::as_str)
        .is_some_and(|socket| {
            !socket.is_empty()
                && crate::host_endpoint_names::socket_names_directory(socket, &name, platform)
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
    fn endpoint_identity_is_written_once_and_repaired_only_when_foreign() {
        let temp = tempfile::tempdir().expect("tempdir");
        let socket = temp.path().join("rpc.sock").to_string_lossy().into_owned();
        let paths = create_host_daemon_paths(&socket, temp.path());
        ensure_endpoint_identity(&paths, &socket, false, None).expect("write");
        let first = fs::read_to_string(&paths.endpoint_file).expect("read");
        let record: serde_json::Value = serde_json::from_str(&first).expect("json");
        assert_eq!(record["registry_version"], ENDPOINT_REGISTRY_VERSION);
        assert_eq!(record["endpoint_kind"], "rpc_host");
        assert_eq!(record["socket"], socket.as_str());
        ensure_endpoint_identity(&paths, &socket, true, None).expect("again");
        assert_eq!(
            fs::read_to_string(&paths.endpoint_file).expect("read"),
            first
        );
        fs::write(&paths.endpoint_file, "{\"socket\":\"/elsewhere\"}").expect("tear");
        ensure_endpoint_identity(&paths, &socket, false, None).expect("no repair");
        assert!(
            fs::read_to_string(&paths.endpoint_file)
                .expect("read")
                .contains("/elsewhere")
        );
        ensure_endpoint_identity(
            &paths,
            &socket,
            true,
            Some(crate::host_endpoints::EndpointKind::Tui),
        )
        .expect("repair");
        let repaired: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&paths.endpoint_file).expect("read"))
                .expect("json");
        assert_eq!(repaired["endpoint_kind"], "tui");
        assert_eq!(
            fs::read_dir(&paths.dir).expect("dir").count(),
            1,
            "no temporary is left behind"
        );
    }
    /// rpc-daemon-directory-modes.test.ts "creates every endpoint directory 0700": the marker and `endpoint.json`
    /// are written by the same call.
    #[cfg(unix)]
    #[test]
    fn creates_every_endpoint_directory_private_with_marker_and_identity() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let agent_dir = temp.path().join("agent");
        let socket = agent_dir
            .join("rpc/shards/p-0000000000000000.sock")
            .to_string_lossy()
            .into_owned();
        let paths = create_host_daemon_paths(&socket, &agent_dir);
        create_daemon_directories(&paths).unwrap();
        for directory in [&paths.dir, &paths.generations_dir, &paths.reservations_dir] {
            assert_eq!(
                fs::metadata(directory).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
        assert!(paths.layout_marker.exists());
        let record: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&paths.endpoint_file).unwrap()).unwrap();
        assert_eq!(
            (
                &record["socket"],
                &record["endpoint_kind"],
                &record["layout"]
            ),
            (
                &serde_json::json!(socket),
                &serde_json::json!("rpc_host"),
                &serde_json::json!(HOST_DAEMON_LAYOUT)
            )
        );
    }
    /// rpc-daemon-directory-modes.test.ts "re-modes only the directories that already existed".
    #[cfg(unix)]
    #[test]
    fn re_modes_directories_that_already_existed() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let paths = create_host_daemon_paths("sock", temp.path());
        fs::create_dir_all(&paths.generations_dir).unwrap();
        for directory in [&paths.dir, &paths.generations_dir] {
            fs::set_permissions(directory, fs::Permissions::from_mode(0o755)).unwrap();
        }
        create_daemon_directories(&paths).unwrap();
        for directory in [&paths.dir, &paths.generations_dir, &paths.reservations_dir] {
            assert_eq!(
                fs::metadata(directory).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
    }
    /// rpc-host-endpoint-identity.test.ts "keeps the first writer's kind": a tui registrant's record survives a later
    /// ensure, repairing or not; a directory-only path set writes no identity.
    #[test]
    fn a_tui_registrant_keeps_its_kind_and_a_directory_only_set_writes_none() {
        let temp = tempfile::tempdir().unwrap();
        let socket = temp
            .path()
            .join("rpc/tui/t-kind.sock")
            .to_string_lossy()
            .into_owned();
        let paths = create_host_daemon_paths(&socket, temp.path());
        create_daemon_directories_as(&paths, Some(crate::host_endpoints::EndpointKind::Tui))
            .unwrap();
        let first = fs::read_to_string(&paths.endpoint_file).unwrap();
        create_daemon_directories(&paths).unwrap();
        ensure_endpoint_identity(&paths, &socket, true, None).unwrap();
        assert_eq!(fs::read_to_string(&paths.endpoint_file).unwrap(), first);
        assert!(first.contains("\"endpoint_kind\":\"tui\""));
        let told =
            host_daemon_directory_paths(&temp.path().join("rpc-host-daemon/0123456789abcdef"));
        create_daemon_directories(&told).unwrap();
        assert!(told.dir.exists() && !told.endpoint_file.exists());
    }
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
