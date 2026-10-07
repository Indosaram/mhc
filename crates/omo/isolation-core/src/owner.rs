use std::path::Path;

use crate::backend::Result;
use crate::process_identity::get_process_start_identity;
use crate::util::{is_denied, mtime_ms, now_ms, random_hex};

pub const OWNER_FILE: &str = ".omo-isolation-owner.json";

const CREATING_GRACE_MS: u64 = 10 * 60_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OwnerStatus {
    Alive,
    Dead,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnerLiveness {
    Live,
    Dead,
    Reclaimable,
    Foreign,
    Retained,
    Creating,
    Unknown,
}

pub trait OwnerProbe: Send + Sync {
    fn pid_alive(&self, pid: u32, start_identity: Option<&str>) -> OwnerStatus;
    fn host_session_alive(&self, _socket: &str, _session_path: &str) -> Option<OwnerStatus> {
        None
    }
}

#[derive(Debug, Clone)]
pub struct HostOwner {
    pub pid: u32,
}

#[derive(Debug, Clone)]
pub enum OwnerChild {
    Process { pid: u32 },
    HostSession { socket: String, session_path: String },
}

#[derive(Debug, Clone)]
pub struct IsolationOwner {
    pub host: HostOwner,
    pub child: Option<OwnerChild>,
}

impl Default for IsolationOwner {
    fn default() -> Self {
        IsolationOwner {
            host: HostOwner {
                pid: std::process::id(),
            },
            child: None,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ProcessOwner {
    pub pid: u32,
    pub start_identity: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum OwnerMarkerChild {
    Process {
        pid: u32,
        start_identity: Option<String>,
    },
    HostSession {
        socket: String,
        session_path: String,
    },
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct OwnerMarker {
    pub id: String,
    pub hostname: String,
    pub created_at: u64,
    pub host: ProcessOwner,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub child: Option<OwnerMarkerChild>,
}

pub fn write_owner_marker(
    base_dir: &Path,
    id: &str,
    owner: &IsolationOwner,
    read_identity: &dyn Fn(u32) -> Option<String>,
) -> Result<()> {
    let host = ProcessOwner {
        pid: owner.host.pid,
        start_identity: read_identity(owner.host.pid),
    };
    let child = match &owner.child {
        Some(OwnerChild::Process { pid }) => Some(OwnerMarkerChild::Process {
            pid: *pid,
            start_identity: read_identity(*pid),
        }),
        Some(OwnerChild::HostSession {
            socket,
            session_path,
        }) => Some(OwnerMarkerChild::HostSession {
            socket: socket.clone(),
            session_path: session_path.clone(),
        }),
        None => None,
    };
    let marker = OwnerMarker {
        id: id.to_string(),
        hostname: hostname(),
        created_at: now_ms(),
        host,
        child,
    };
    let temporary = base_dir.join(format!("{OWNER_FILE}.{}.tmp", random_hex(16)));
    let text = serde_json::to_string(&marker)
        .map_err(|error| crate::backend::IsolationError::other(error.to_string()))?;
    write_private_exclusive(&temporary, &text)?;
    std::fs::rename(&temporary, base_dir.join(OWNER_FILE))?;
    Ok(())
}

pub fn write_owner_marker_default(base_dir: &Path, id: &str, owner: &IsolationOwner) -> Result<()> {
    write_owner_marker(base_dir, id, owner, &get_process_start_identity)
}

fn write_private_exclusive(path: &Path, text: &str) -> std::io::Result<()> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(text.as_bytes())?;
    Ok(())
}

pub fn hostname() -> String {
    #[cfg(unix)]
    {
        let mut buffer = [0u8; 256];
        let result = unsafe {
            libc::gethostname(
                buffer.as_mut_ptr() as *mut libc::c_char,
                buffer.len() as libc::size_t,
            )
        };
        if result == 0 {
            let end = buffer
                .iter()
                .position(|byte| *byte == 0)
                .unwrap_or(buffer.len());
            if let Ok(name) = std::str::from_utf8(&buffer[..end]) && !name.is_empty() {
                return name.to_string();
            }
        }
    }
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_default()
}

fn is_process_owner(value: &serde_json::Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    let Some(pid) = object.get("pid").and_then(|pid| pid.as_u64()) else {
        return false;
    };
    if pid == 0 || pid > 9_007_199_254_740_991 {
        return false;
    }
    match object.get("start_identity") {
        Some(identity) => identity.is_null() || identity.is_string(),
        None => false,
    }
}

fn is_owner_marker(value: &serde_json::Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    if !object.get("id").map(|id| id.is_string()).unwrap_or(false) {
        return false;
    }
    if !object
        .get("hostname")
        .map(|hostname| hostname.is_string())
        .unwrap_or(false)
    {
        return false;
    }
    if !object
        .get("created_at")
        .and_then(|created| created.as_f64())
        .map(|created| created.is_finite())
        .unwrap_or(false)
    {
        return false;
    }
    if !object.get("host").map(is_process_owner).unwrap_or(false) {
        return false;
    }
    let Some(child) = object.get("child") else {
        return true;
    };
    let Some(child) = child.as_object() else {
        return false;
    };
    match child.get("kind").and_then(|kind| kind.as_str()) {
        Some("process") => is_process_owner(&serde_json::Value::Object(child.clone())),
        Some("host-session") => {
            child.get("socket").map(|socket| socket.is_string()).unwrap_or(false)
                && child
                    .get("session_path")
                    .map(|session| session.is_string())
                    .unwrap_or(false)
        }
        _ => false,
    }
}

pub fn read_owner_liveness(base_dir: &Path, probe: &dyn OwnerProbe, now: u64) -> Result<OwnerLiveness> {
    let name = base_dir
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    if name.contains(".retained-") {
        return Ok(OwnerLiveness::Retained);
    }
    let marker_path = base_dir.join(OWNER_FILE);
    let marker_text = match std::fs::read_to_string(&marker_path) {
        Ok(text) => Some(text),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) if is_denied(&error) => return Ok(OwnerLiveness::Unknown),
        Err(error) => return Err(error.into()),
    };
    let marker: Option<serde_json::Value> = match marker_text {
        Some(text) => serde_json::from_str(&text).ok(),
        None => None,
    };
    if let Some(value) = &marker
        && let Some(recorded_host) = value.get("hostname").and_then(|host| host.as_str())
        && recorded_host != hostname()
    {
        return Ok(OwnerLiveness::Foreign);
    }
    if let Some(value) = &marker && is_owner_marker(value) {
            let parsed: OwnerMarker = serde_json::from_value(value.clone())
                .map_err(|error| crate::backend::IsolationError::other(error.to_string()))?;
            let mut states = vec![probe.pid_alive(
                parsed.host.pid,
                parsed.host.start_identity.as_deref(),
            )];
            if let Some(child) = &parsed.child {
                match child {
                    OwnerMarkerChild::Process {
                        pid,
                        start_identity,
                    } => states.push(probe.pid_alive(*pid, start_identity.as_deref())),
                    OwnerMarkerChild::HostSession {
                        socket,
                        session_path,
                    } => states.push(
                        probe
                            .host_session_alive(socket, session_path)
                            .unwrap_or(OwnerStatus::Unknown),
                    ),
                }
            }
            if states.contains(&OwnerStatus::Alive) {
                return Ok(if name.contains(".creating-") {
                    OwnerLiveness::Creating
                } else {
                    OwnerLiveness::Live
                });
            }
            if states.contains(&OwnerStatus::Unknown) {
                return Ok(OwnerLiveness::Unknown);
            }
            if !name.contains(".creating-") {
                return Ok(OwnerLiveness::Dead);
            }
    }
    let age = now.saturating_sub(mtime_ms(base_dir)?);
    Ok(if age > CREATING_GRACE_MS {
        OwnerLiveness::Reclaimable
    } else {
        OwnerLiveness::Creating
    })
}

pub fn read_owner_liveness_now(base_dir: &Path, probe: &dyn OwnerProbe) -> Result<OwnerLiveness> {
    read_owner_liveness(base_dir, probe, now_ms())
}
