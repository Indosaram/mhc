//! Port of `ownership.ts`: startup lease, owner metadata and dead-owner reclamation.

use std::fmt;
use std::future::Future;
use std::io;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use crate::ipc_protocol::{
    ensure_private_directory, read_auth_token, read_or_create_auth_token, rotate_auth_token,
    write_private_file,
};
use crate::lock::{LockHandle, is_process_alive, try_acquire_lock, unlink_quietly};
use crate::paths::DaemonPaths;
use crate::platform;

/// TS `EndpointIdentity`: which socket inode (or pipe name) an owner listens on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EndpointIdentity {
    Unix { path: String, dev: u64, ino: u64 },
    Windows { path: String },
    Missing { path: String },
}

impl EndpointIdentity {
    pub fn path(&self) -> &str {
        match self {
            Self::Unix { path, .. } | Self::Windows { path } | Self::Missing { path } => path,
        }
    }

    pub fn to_json(&self) -> Value {
        match self {
            Self::Unix { path, dev, ino } => {
                json!({"kind": "unix", "path": path, "dev": dev, "ino": ino})
            }
            Self::Windows { path } => json!({"kind": "windows", "path": path}),
            Self::Missing { path } => json!({"kind": "missing", "path": path}),
        }
    }

    /// Strict wire parser used for ping responses (TS `parsePingResponse` endpoint branch).
    pub fn from_json_strict(value: &Value) -> Option<Self> {
        let record = value.as_object()?;
        let path = record.get("path")?.as_str()?.to_string();
        match record.get("kind").and_then(Value::as_str) {
            Some("windows") => Some(Self::Windows { path }),
            Some("missing") => Some(Self::Missing { path }),
            Some("unix") => Some(Self::Unix {
                path,
                dev: record.get("dev")?.as_u64()?,
                ino: record.get("ino")?.as_u64()?,
            }),
            _ => None,
        }
    }

    /// Owner-file parser: a missing `kind` means legacy metadata, re-identified from disk.
    fn from_owner_json(value: &Value) -> Option<Self> {
        let record = value.as_object()?;
        let path = record.get("path")?.as_str()?;
        if record.get("kind").is_none() {
            return Some(endpoint_identity(path));
        }
        Self::from_json_strict(value)
    }
}

/// TS `DaemonOwner` / `OwnerPing` (same shape).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonOwner {
    pub pid: u32,
    pub nonce: String,
    pub started_at: String,
    pub endpoint: EndpointIdentity,
}

pub type OwnerPing = DaemonOwner;

impl DaemonOwner {
    pub fn to_json(&self) -> Value {
        json!({
            "pid": self.pid,
            "nonce": self.nonce,
            "startedAt": self.started_at,
            "endpoint": self.endpoint.to_json(),
        })
    }

    fn parse(
        value: &Value,
        endpoint: impl FnOnce(&Value) -> Option<EndpointIdentity>,
    ) -> Option<Self> {
        let record = value.as_object()?;
        Some(Self {
            pid: u32::try_from(record.get("pid")?.as_u64()?).ok()?,
            nonce: record.get("nonce")?.as_str()?.to_string(),
            started_at: record.get("startedAt")?.as_str()?.to_string(),
            endpoint: endpoint(record.get("endpoint")?)?,
        })
    }

    /// TS `parsePingResponse`: the `result` of an `omo/ping` reply.
    pub fn from_ping_response(message: &Value) -> Option<Self> {
        Self::parse(message.get("result")?, EndpointIdentity::from_json_strict)
    }
}

/// Lock + token + prospective owner held while a candidate daemon binds.
#[derive(Debug)]
pub struct StartupLease {
    pub lock: LockHandle,
    pub token: String,
    pub owner: DaemonOwner,
}

/// Why a candidate daemon did not take ownership.
#[derive(Debug)]
pub enum StartupError {
    /// TS `DaemonAlreadyRunningError` (`daemon_already_running`).
    AlreadyRunning,
    /// TS `DaemonStartupDeferredError` (`daemon_startup_deferred`).
    Deferred(&'static str),
    Io(io::Error),
}

impl StartupError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::AlreadyRunning => "daemon_already_running",
            Self::Deferred(_) => "daemon_startup_deferred",
            Self::Io(_) => "io_error",
        }
    }
}

impl fmt::Display for StartupError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AlreadyRunning => formatter.write_str("LSP daemon already running"),
            Self::Deferred(reason) => write!(formatter, "LSP daemon startup deferred: {reason}"),
            Self::Io(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for StartupError {}

impl From<io::Error> for StartupError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// TS `acquireStartupLease`.
pub async fn acquire_startup_lease<F, Fut>(
    paths: &DaemonPaths,
    ping_owner: F,
) -> Result<StartupLease, StartupError>
where
    F: Fn(String) -> Fut,
    Fut: Future<Output = Option<OwnerPing>>,
{
    ensure_daemon_directories(paths)?;
    let Some(lock) = try_acquire_lock(&paths.lock, std::process::id())? else {
        if let Some(token) = read_auth_token(paths)
            && ping_owner(token).await.is_some()
        {
            return Err(StartupError::AlreadyRunning);
        }
        return Err(StartupError::Deferred("startup_lock_busy"));
    };
    let validated = match validate_existing_owner(paths, &ping_owner).await {
        Ok(token) => new_owner(paths)
            .map(|owner| (token, owner))
            .map_err(StartupError::Io),
        Err(error) => Err(error),
    };
    match validated {
        Ok((token, owner)) => Ok(StartupLease { lock, token, owner }),
        Err(error) => {
            lock.release();
            Err(error)
        }
    }
}

pub fn ensure_daemon_directories(paths: &DaemonPaths) -> io::Result<()> {
    ensure_private_directory(&paths.dir)?;
    if !cfg!(windows)
        && let Some(parent) = paths.socket.parent()
    {
        ensure_private_directory(parent)?;
    }
    Ok(())
}

pub fn read_daemon_owner(paths: &DaemonPaths) -> Option<DaemonOwner> {
    let text = std::fs::read_to_string(&paths.owner).ok()?;
    let value: Value = serde_json::from_str(&text).ok()?;
    DaemonOwner::parse(&value, EndpointIdentity::from_owner_json)
}

pub fn write_daemon_owner(paths: &DaemonPaths, owner: &DaemonOwner) -> io::Result<()> {
    write_private_file(&paths.pid, &format!("{}\n", owner.pid))?;
    write_private_file(&paths.endpoint, owner.endpoint.path())?;
    write_private_file(&paths.owner, &format!("{}\n", owner.to_json()))
}

/// TS `removeDaemonMetadataForOwner`: only the current owner may remove the metadata.
pub fn remove_daemon_metadata_for_owner(paths: &DaemonPaths, owner: &DaemonOwner) {
    let Some(current) = read_daemon_owner(paths) else {
        return;
    };
    if !same_owner(&current, owner) {
        return;
    }
    unlink_quietly(&paths.socket);
    unlink_quietly(&paths.pid);
    unlink_quietly(&paths.endpoint);
    unlink_quietly(&paths.owner);
}

pub fn endpoint_identity(endpoint_path: &str) -> EndpointIdentity {
    let path = endpoint_path.to_string();
    if cfg!(windows) {
        return EndpointIdentity::Windows { path };
    }
    match platform::path_identity(Path::new(endpoint_path)) {
        Ok((dev, ino)) => EndpointIdentity::Unix { path, dev, ino },
        Err(_) => EndpointIdentity::Missing { path },
    }
}

pub fn same_endpoint(a: &EndpointIdentity, b: &EndpointIdentity) -> bool {
    match (a, b) {
        (
            EndpointIdentity::Unix { path, dev, ino },
            EndpointIdentity::Unix {
                path: other_path,
                dev: other_dev,
                ino: other_ino,
            },
        ) => path == other_path && dev == other_dev && ino == other_ino,
        (EndpointIdentity::Windows { path }, EndpointIdentity::Windows { path: other })
        | (EndpointIdentity::Missing { path }, EndpointIdentity::Missing { path: other }) => {
            path == other
        }
        (
            EndpointIdentity::Unix { .. }
            | EndpointIdentity::Windows { .. }
            | EndpointIdentity::Missing { .. },
            _,
        ) => false,
    }
}

pub fn same_owner(a: &DaemonOwner, b: &DaemonOwner) -> bool {
    a.pid == b.pid && a.nonce == b.nonce && same_endpoint(&a.endpoint, &b.endpoint)
}

async fn validate_existing_owner<F, Fut>(
    paths: &DaemonPaths,
    ping_owner: &F,
) -> Result<String, StartupError>
where
    F: Fn(String) -> Fut,
    Fut: Future<Output = Option<OwnerPing>>,
{
    let token = read_or_create_auth_token(paths)?;
    for _attempt in 0..2 {
        let Some(owner) = read_daemon_owner(paths) else {
            return Ok(token);
        };
        let ping = ping_owner(token.clone()).await;
        if let Some(ping) = &ping {
            if owner.nonce == ping.nonce && same_endpoint(&owner.endpoint, &ping.endpoint) {
                return Err(StartupError::AlreadyRunning);
            }
            continue;
        }
        if is_process_alive(i64::from(owner.pid)) {
            return Err(StartupError::Deferred("owner_pid_live_unreachable"));
        }
        let reread = read_daemon_owner(paths);
        let endpoint = endpoint_identity(owner.endpoint.path());
        let unchanged = reread.is_some_and(|reread| reread.nonce == owner.nonce)
            && same_endpoint(&endpoint, &owner.endpoint);
        if !unchanged {
            return Err(StartupError::Deferred("owner_changed_during_cleanup"));
        }
        cleanup_dead_owner(paths, &owner);
        return Ok(rotate_auth_token(paths)?);
    }
    Err(StartupError::Deferred("reachable_owner_mismatch"))
}

fn cleanup_dead_owner(paths: &DaemonPaths, owner: &DaemonOwner) {
    let endpoint = Path::new(owner.endpoint.path());
    if !cfg!(windows) && platform::is_socket(endpoint) {
        unlink_quietly(endpoint);
    }
    unlink_quietly(&paths.pid);
    unlink_quietly(&paths.endpoint);
    unlink_quietly(&paths.owner);
}

fn new_owner(paths: &DaemonPaths) -> io::Result<DaemonOwner> {
    Ok(DaemonOwner {
        pid: std::process::id(),
        nonce: random_uuid()?,
        started_at: iso_timestamp(SystemTime::now()),
        endpoint: endpoint_identity(&paths.socket.to_string_lossy()),
    })
}

/// TS `crypto.randomUUID`: RFC 4122 version 4.
pub fn random_uuid() -> io::Result<String> {
    let mut bytes = platform::random_bytes(16)?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex = crate::crypto::hex_lower(&bytes);
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    ))
}

/// TS `Date.prototype.toISOString`.
pub fn iso_timestamp(time: SystemTime) -> String {
    let since = time.duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = i64::try_from(since.as_secs()).unwrap_or(i64::MAX);
    let millis = since.subsec_millis();
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{millis:03}Z",
        rem / 3_600,
        (rem % 3_600) / 60,
        rem % 60
    )
}
