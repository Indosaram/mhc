use std::path::PathBuf;
use std::time::Duration;

use serde_json::{Map, Value, json};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LspDaemonOwnerEndpoint {
    pub dev: i64,
    pub ino: i64,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LspDaemonOwnerIdentity {
    pub endpoint: LspDaemonOwnerEndpoint,
    pub nonce: String,
    pub pid: u32,
    pub started_at: String,
}

impl LspDaemonOwnerIdentity {
    /// Wire shape written by packages/lsp-daemon/src/ownership.ts.
    pub fn to_json(&self) -> Value {
        json!({
            "endpoint": { "dev": self.endpoint.dev, "ino": self.endpoint.ino, "kind": "unix", "path": self.endpoint.path },
            "nonce": self.nonce,
            "pid": self.pid,
            "startedAt": self.started_at,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LspDaemonOwnerTarget {
    pub auth_path: PathBuf,
    pub owner: LspDaemonOwnerIdentity,
    pub owner_path: PathBuf,
    pub pid: u32,
}

const OWNER_PING_REQUEST_ID: &str = "process-sweep-owner-attestation";
const OMO_DAEMON_PROTOCOL_VERSION: u32 = 1;
const OWNER_PING_TIMEOUT: Duration = Duration::from_millis(500);

pub type LspDaemonOwnerPing<'a> =
    &'a dyn Fn(&LspDaemonOwnerEndpoint, &str) -> Option<LspDaemonOwnerIdentity>;
pub type LspDaemonReadText<'a> = &'a dyn Fn(&std::path::Path) -> Result<String, String>;

#[derive(Default)]
pub struct LspDaemonOwnerAttestationDeps<'a> {
    pub ping_owner: Option<LspDaemonOwnerPing<'a>>,
    pub read_text: Option<LspDaemonReadText<'a>>,
}

/// Proves the owner record is unchanged and the live daemon answers an
/// authenticated `omo/ping` with the same identity. Any doubt returns false.
pub fn attest_lsp_daemon_owner(
    target: &LspDaemonOwnerTarget,
    deps: &LspDaemonOwnerAttestationDeps<'_>,
) -> bool {
    let default_read =
        |path: &std::path::Path| std::fs::read_to_string(path).map_err(|error| error.to_string());
    let read_text: LspDaemonReadText<'_> = deps.read_text.unwrap_or(&default_read);
    let Ok(owner_text) = read_text(&target.owner_path) else {
        return false;
    };
    let current_owner = serde_json::from_str::<Value>(&owner_text)
        .ok()
        .and_then(|value| parse_lsp_daemon_owner(&value));
    if current_owner.as_ref() != Some(&target.owner) {
        return false;
    }
    let Ok(auth_text) = read_text(&target.auth_path) else {
        return false;
    };
    let auth_token = auth_text.trim();
    if auth_token.is_empty() {
        return false;
    }
    let live_owner = match deps.ping_owner {
        Some(ping) => ping(&target.owner.endpoint, auth_token),
        None => ping_lsp_daemon_owner(&target.owner.endpoint, auth_token),
    };
    live_owner.as_ref() == Some(&target.owner)
}

pub fn parse_lsp_daemon_owner(value: &Value) -> Option<LspDaemonOwnerIdentity> {
    let record = value.as_object()?;
    let pid = record
        .get("pid")?
        .as_u64()
        .and_then(|pid| u32::try_from(pid).ok())
        .filter(|pid| *pid > 0)?;
    let nonce = non_empty_str(record, "nonce")?;
    let started_at = non_empty_str(record, "startedAt")?;
    let endpoint = record.get("endpoint")?.as_object()?;
    if endpoint.get("kind")?.as_str()? != "unix" {
        return None;
    }
    let path = non_empty_str(endpoint, "path")?;
    let dev = endpoint.get("dev")?.as_i64()?;
    let ino = endpoint.get("ino")?.as_i64()?;
    Some(LspDaemonOwnerIdentity {
        endpoint: LspDaemonOwnerEndpoint { dev, ino, path },
        nonce,
        pid,
        started_at,
    })
}

fn non_empty_str(record: &Map<String, Value>, key: &str) -> Option<String> {
    record
        .get(key)?
        .as_str()
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

pub fn create_lsp_daemon_owner_ping_request(auth_token: &str) -> Value {
    json!({
        "id": OWNER_PING_REQUEST_ID,
        "jsonrpc": "2.0",
        "method": "omo/ping",
        "params": { "_omo": { "protocolVersion": OMO_DAEMON_PROTOCOL_VERSION, "token": auth_token } },
    })
}

#[cfg(unix)]
fn ping_lsp_daemon_owner(
    endpoint: &LspDaemonOwnerEndpoint,
    auth_token: &str,
) -> Option<LspDaemonOwnerIdentity> {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;

    let mut stream = UnixStream::connect(&endpoint.path).ok()?;
    stream.set_read_timeout(Some(OWNER_PING_TIMEOUT)).ok()?;
    stream.set_write_timeout(Some(OWNER_PING_TIMEOUT)).ok()?;
    let request = create_lsp_daemon_owner_ping_request(auth_token);
    stream.write_all(format!("{request}\n").as_bytes()).ok()?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).ok()?;
    let response: Value = serde_json::from_str(line.trim()).ok()?;
    let record = response.as_object()?;
    if record.get("id")?.as_str()? != OWNER_PING_REQUEST_ID || record.contains_key("error") {
        return None;
    }
    parse_lsp_daemon_owner(record.get("result")?)
}

#[cfg(not(unix))]
fn ping_lsp_daemon_owner(
    _endpoint: &LspDaemonOwnerEndpoint,
    _auth_token: &str,
) -> Option<LspDaemonOwnerIdentity> {
    None
}
