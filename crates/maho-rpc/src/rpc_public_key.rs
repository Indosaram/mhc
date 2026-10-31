//! Per-launch public endpoint keys (senpi `rpc-public-key.ts`, #3795). A POSIX public host owns one in-memory key per
//! launch, publishes it once after bind as `<agentDir>/rpc/keys/<launchId>` (0600, in a 0700 directory), removes it
//! on dispose, and `host gc` collects records whose process is provably gone. Reading a record is
//! `socket_auth::read_key`.
#![cfg(unix)]
use crate::socket_auth::RpcIdentityError;
use serde_json::{Value, json};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

fn unsafe_record(path: &Path, detail: impl Into<String>) -> RpcIdentityError {
    RpcIdentityError {
        code: "unsafe_key_record",
        location: path.display().to_string(),
        detail: detail.into(),
    }
}

/// senpi `RPC_LAUNCH_ID`: a lowercase v4 uuid.
pub fn is_launch_id(value: &str) -> bool {
    uuid::Uuid::parse_str(value)
        .is_ok_and(|id| id.get_version_num() == 4 && id.hyphenated().to_string() == value)
}

/// senpi `checkRpcKeyDirectory`: `<root>` and `<root>/rpc` at their realpath, owned by this user without group/other
/// write; `<root>/rpc/keys` itself a real directory owned by this user with mode exactly 0700.
fn check_directory(path: &Path, private_keys: bool) -> Result<(), RpcIdentityError> {
    let checked = if private_keys {
        path.to_path_buf()
    } else {
        std::fs::canonicalize(path).map_err(|error| unsafe_record(path, error.to_string()))?
    };
    let info = std::fs::symlink_metadata(&checked)
        .map_err(|error| unsafe_record(&checked, error.to_string()))?;
    let shown = checked.display();
    if !info.is_dir() {
        return Err(unsafe_record(
            &checked,
            format!("{shown} is not a directory"),
        ));
    }
    if info.uid() != rustix::process::getuid().as_raw() {
        return Err(unsafe_record(
            &checked,
            format!("{shown} is not owned by this user"),
        ));
    }
    let mode = info.mode() & 0o7777;
    if private_keys && mode != 0o700 {
        return Err(unsafe_record(
            &checked,
            format!("{shown} must be mode 0700 (is 0{mode:o}); run: chmod 700 {shown}"),
        ));
    }
    if !private_keys && mode & 0o022 != 0 {
        return Err(unsafe_record(
            &checked,
            format!("{shown} is writable by group or others; run: chmod go-w {shown}"),
        ));
    }
    Ok(())
}

fn check_parents(root: &Path) -> Result<(), RpcIdentityError> {
    check_directory(root, false)?;
    check_directory(&root.join("rpc"), false)
}

/// One public binder owns one in-memory key; it never reads a key back from disk (senpi `RpcPublicEndpoint`).
pub struct RpcPublicEndpoint {
    pub key: [u8; 32],
    pub launch_id: String,
    pub record_path: PathBuf,
    agent_dir: PathBuf,
    endpoint: String,
    created: std::sync::atomic::AtomicBool,
}

impl RpcPublicEndpoint {
    pub fn new(agent_dir: &Path, endpoint: &str) -> Result<Self, RpcIdentityError> {
        let mut key = [0u8; 32];
        getrandom::fill(&mut key).map_err(|error| RpcIdentityError {
            code: "key_publication_failed",
            location: endpoint.to_owned(),
            detail: error.to_string(),
        })?;
        let launch_id = uuid::Uuid::new_v4().hyphenated().to_string();
        Ok(Self {
            key,
            record_path: agent_dir.join("rpc").join("keys").join(&launch_id),
            launch_id,
            agent_dir: agent_dir.to_path_buf(),
            endpoint: endpoint.to_owned(),
            created: std::sync::atomic::AtomicBool::new(false),
        })
    }

    /// The public endpoint this key authenticates (senpi `endpoint`).
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// Writes the key record once, only after the listener is bound (senpi `publish`); every failure is
    /// `key_publication_failed` naming what was unsafe.
    pub fn publish(&self) -> Result<(), RpcIdentityError> {
        self.write_record().map_err(|cause| RpcIdentityError {
            code: "key_publication_failed",
            location: cause.location.clone(),
            detail: cause.to_string(),
        })
    }

    fn write_record(&self) -> Result<(), RpcIdentityError> {
        use std::io::Write;
        fn io(path: &Path) -> impl FnOnce(std::io::Error) -> RpcIdentityError + '_ {
            move |error| unsafe_record(path, error.to_string())
        }
        let rpc = self.agent_dir.join("rpc");
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&rpc)
            .map_err(io(&rpc))?;
        check_parents(&self.agent_dir)?;
        let keys = rpc.join("keys");
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&keys)
            .map_err(io(&keys))?;
        // An existing keys directory this user owns is tightened; a symlink or a foreign one is refused.
        let directory = std::fs::symlink_metadata(&keys).map_err(io(&keys))?;
        if !directory.is_dir() || directory.uid() != rustix::process::getuid().as_raw() {
            return Err(unsafe_record(&keys, "not a directory owned by this user"));
        }
        if directory.mode() & 0o7777 != 0o700 {
            std::fs::set_permissions(&keys, std::fs::Permissions::from_mode(0o700))
                .map_err(io(&keys))?;
        }
        check_directory(&keys, true)?;
        let path = &self.record_path;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
            .map_err(io(path))?;
        self.created
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let record = json!({
            "version": 1,
            "launch_id": self.launch_id,
            "key": hex::encode(self.key),
            "pid": std::process::id(),
            "process_start": crate::host_reservations::read_process_start_time(std::process::id()),
            "endpoint": self.endpoint,
            "created_at": chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        });
        file.write_all(record.to_string().as_bytes())
            .and_then(|()| file.sync_all())
            .map_err(io(path))?;
        let info = file.metadata().map_err(io(path))?;
        if !info.is_file()
            || info.uid() != rustix::process::getuid().as_raw()
            || info.mode() & 0o7777 != 0o600
        {
            return Err(unsafe_record(
                path,
                "not a 0600 regular file owned by this user",
            ));
        }
        Ok(())
    }

    /// Removes this launch's own record (senpi `dispose`); a record already gone is fine.
    pub fn dispose(&self) -> std::io::Result<()> {
        if !self
            .created
            .swap(false, std::sync::atomic::Ordering::SeqCst)
        {
            return Ok(());
        }
        match std::fs::remove_file(&self.record_path) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        }
    }
}

impl Drop for RpcPublicEndpoint {
    /// senpi's `process.once("exit")` cleanup: the record never outlives its binder.
    fn drop(&mut self) {
        if self.dispose().is_err() {
            eprintln!("senpi rpc host: could not remove own endpoint key record");
        }
    }
}

/// How far one gc pass may go (senpi `{ deadline, maxRecords }`).
#[derive(Debug, Clone, Copy, Default)]
pub struct KeyGcBudget {
    pub deadline: Option<std::time::Instant>,
    pub max_records: Option<usize>,
}

/// senpi `gcRpcPublicKeys`: unknown process identity keeps a record; only death or observed pid reuse permits
/// collection. An unsafe key root is left alone.
pub fn gc_rpc_public_keys(agent_dir: &Path, budget: KeyGcBudget) -> std::io::Result<()> {
    let keys = agent_dir.join("rpc").join("keys");
    if check_parents(agent_dir).is_err() || check_directory(&keys, true).is_err() {
        return Ok(());
    }
    let names = match std::fs::read_dir(&keys) {
        Ok(names) => names,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    let mut examined = 0usize;
    for name in names {
        let name = name?.file_name().to_string_lossy().into_owned();
        if budget
            .deadline
            .is_some_and(|deadline| std::time::Instant::now() >= deadline)
            || budget.max_records.is_some_and(|max| examined >= max)
        {
            break;
        }
        if !is_launch_id(&name) {
            continue;
        }
        examined += 1;
        let Some(record) = read_record(&keys.join(&name)) else {
            continue;
        };
        let Some(pid) = record["pid"]
            .as_u64()
            .and_then(|pid| u32::try_from(pid).ok())
            .filter(|pid| *pid > 0)
        else {
            continue;
        };
        let recorded = match &record["process_start"] {
            Value::Null => None,
            Value::String(start) => Some(start.as_str()),
            _ => continue,
        };
        if process_may_match(pid, recorded) {
            continue;
        }
        match std::fs::remove_file(keys.join(&name)) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(error),
            _ => {}
        }
    }
    Ok(())
}

/// The record behind a launch id, read the way `socket_auth::read_key` checks it; `None` when unsafe or unreadable.
fn read_record(path: &Path) -> Option<Value> {
    let launch = path.file_name()?.to_str()?;
    let root = path.parent()?.parent()?.parent()?;
    crate::socket_auth::read_key(root, launch).ok()??;
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

/// senpi `processMatchesPidFile`: a dead pid never matches; a live one matches unless its observed start differs from
/// the recorded one. Unknown identity (no record or unreadable now) keeps the record.
fn process_may_match(pid: u32, recorded: Option<&str>) -> bool {
    if !crate::host_reservations::process_is_running(pid) {
        return false;
    }
    match (
        recorded,
        crate::host_reservations::read_process_start_time(pid).as_deref(),
    ) {
        (Some(recorded), Some(observed)) => recorded == observed,
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn root() -> tempfile::TempDir {
        let temp = tempfile::tempdir().unwrap();
        std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        temp
    }
    #[test]
    fn publish_writes_a_private_record_that_reads_back_and_dispose_removes_it() {
        let temp = root();
        let endpoint = RpcPublicEndpoint::new(temp.path(), "/tmp/host.sock").unwrap();
        assert!(is_launch_id(&endpoint.launch_id));
        endpoint.publish().unwrap();
        let mode = std::fs::metadata(&endpoint.record_path).unwrap().mode() & 0o7777;
        assert_eq!(mode, 0o600);
        let key = crate::socket_auth::read_key(temp.path(), &endpoint.launch_id)
            .unwrap()
            .unwrap();
        assert_eq!(key, endpoint.key);
        // A live record of this very process is never collected.
        gc_rpc_public_keys(temp.path(), KeyGcBudget::default()).unwrap();
        assert!(endpoint.record_path.exists());
        endpoint.dispose().unwrap();
        assert!(!endpoint.record_path.exists());
    }
    #[test]
    fn gc_collects_a_record_whose_process_start_changed() {
        let temp = root();
        let endpoint = RpcPublicEndpoint::new(temp.path(), "/tmp/host.sock").unwrap();
        endpoint.publish().unwrap();
        let mut record: Value =
            serde_json::from_slice(&std::fs::read(&endpoint.record_path).unwrap()).unwrap();
        record["process_start"] = "not-this-start".into();
        std::fs::write(&endpoint.record_path, record.to_string()).unwrap();
        gc_rpc_public_keys(temp.path(), KeyGcBudget::default()).unwrap();
        assert!(!endpoint.record_path.exists());
    }
    #[test]
    fn a_group_writable_root_refuses_publication() {
        let temp = root();
        std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o775)).unwrap();
        let endpoint = RpcPublicEndpoint::new(temp.path(), "/tmp/host.sock").unwrap();
        let error = endpoint.publish().unwrap_err();
        assert_eq!(error.code, "key_publication_failed");
    }
}
