//! Port of `ipc-protocol.ts`: auth envelope, token files and private state directories.

use std::fmt;
use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use crate::crypto::{constant_time_eq, random_base64url};
use crate::paths::DaemonPaths;
use crate::platform;

pub const OMO_DAEMON_PROTOCOL_VERSION: u64 = 1;
pub const AUTH_ERROR_CODE: i64 = -32001;
pub const PROTOCOL_ERROR_CODE: i64 = -32002;
const AUTH_TOKEN_BYTES: usize = 32;

/// TS `authEnvelope`: the `params._omo` object every daemon request carries.
pub fn auth_envelope(token: &str) -> Value {
    json!({ "protocolVersion": OMO_DAEMON_PROTOCOL_VERSION, "token": token })
}

/// A request whose envelope was verified and stripped (TS `AuthenticatedMessage`).
#[derive(Debug, Clone, PartialEq)]
pub struct AuthenticatedMessage {
    pub input: Map<String, Value>,
    pub id: Value,
    pub method: Option<String>,
}

/// TS `authenticateMessage`: `Ok` with the cleaned request, or the JSON-RPC error to send.
pub fn authenticate_message(
    raw: &Value,
    expected_token: &str,
) -> Result<AuthenticatedMessage, Value> {
    let id = json_rpc_id(raw);
    let Some(record) = raw.as_object() else {
        return Err(auth_error(id));
    };
    let Some(params) = record.get("params").and_then(Value::as_object) else {
        return Err(auth_error(id));
    };
    let Some(envelope) = params.get("_omo").and_then(Value::as_object) else {
        return Err(auth_error(id));
    };
    if envelope.get("protocolVersion") != Some(&json!(OMO_DAEMON_PROTOCOL_VERSION)) {
        return Err(protocol_error(id));
    }
    let token_ok = envelope
        .get("token")
        .and_then(Value::as_str)
        .is_some_and(|token| constant_time_eq(token.as_bytes(), expected_token.as_bytes()));
    if !token_ok {
        return Err(auth_error(id));
    }
    let mut clean_params = params.clone();
    clean_params.remove("_omo");
    let mut input = record.clone();
    input.insert("params".to_string(), Value::Object(clean_params));
    Ok(AuthenticatedMessage {
        input,
        method: record
            .get("method")
            .and_then(Value::as_str)
            .map(str::to_string),
        id,
    })
}

/// TS `isAuthErrorResponse`.
pub fn is_auth_error_response(message: &Value) -> bool {
    let Some(error) = message.get("error").and_then(Value::as_object) else {
        return false;
    };
    error.get("code") == Some(&json!(AUTH_ERROR_CODE))
        && error
            .get("data")
            .and_then(|data| data.get("code"))
            .and_then(Value::as_str)
            == Some("daemon_authentication_failed")
}

/// Id echo rule: strings, numbers and null pass through; everything else is null.
pub fn json_rpc_id(raw: &Value) -> Value {
    match raw.get("id") {
        Some(id @ (Value::String(_) | Value::Number(_))) => id.clone(),
        _ => Value::Null,
    }
}

fn auth_error(id: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": AUTH_ERROR_CODE,
            "message": "daemon authentication failed",
            "data": { "code": "daemon_authentication_failed" }
        }
    })
}

fn protocol_error(id: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": PROTOCOL_ERROR_CODE,
            "message": "daemon protocol mismatch",
            "data": { "code": "daemon_protocol_mismatch" }
        }
    })
}

pub fn read_auth_token(paths: &DaemonPaths) -> Option<String> {
    let token = std::fs::read_to_string(&paths.auth).ok()?;
    let token = token.trim();
    (!token.is_empty()).then(|| token.to_string())
}

pub fn read_or_create_auth_token(paths: &DaemonPaths) -> io::Result<String> {
    match read_auth_token(paths) {
        Some(token) => Ok(token),
        None => create_auth_token(paths),
    }
}

pub fn rotate_auth_token(paths: &DaemonPaths) -> io::Result<String> {
    let _ignored = std::fs::remove_file(&paths.auth);
    create_auth_token(paths)
}

fn create_auth_token(paths: &DaemonPaths) -> io::Result<String> {
    if let Some(parent) = paths.auth.parent() {
        ensure_private_directory(parent)?;
    }
    let token = random_base64url(AUTH_TOKEN_BYTES)?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = match options.open(&paths.auth) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            return read_auth_token(paths).ok_or(error);
        }
        Err(error) => return Err(error),
    };
    file.write_all(format!("{token}\n").as_bytes())?;
    drop(file);
    platform::set_private_file_mode(&paths.auth)?;
    Ok(token)
}

/// TS `writePrivateFile`: truncating write followed by a 0600 chmod.
pub fn write_private_file(path: &Path, data: &str) -> io::Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)?.write_all(data.as_bytes())?;
    platform::set_private_file_mode(path)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnsafeDirectoryReason {
    ChangedDuringChmod,
    NotDirectory,
    Symlink,
    WrongOwner,
}

impl UnsafeDirectoryReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ChangedDuringChmod => "changed_during_chmod",
            Self::NotDirectory => "not_directory",
            Self::Symlink => "symlink",
            Self::WrongOwner => "wrong_owner",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnsafePrivateDirectoryError {
    pub path: PathBuf,
    pub reason: UnsafeDirectoryReason,
}

impl UnsafePrivateDirectoryError {
    pub const CODE: &'static str = "unsafe_private_directory";
}

impl fmt::Display for UnsafePrivateDirectoryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "unsafe private directory {}: {}",
            self.path.display(),
            self.reason.as_str()
        )
    }
}

impl std::error::Error for UnsafePrivateDirectoryError {}

fn unsafe_dir(path: &Path, reason: UnsafeDirectoryReason) -> io::Error {
    io::Error::other(UnsafePrivateDirectoryError {
        path: path.to_path_buf(),
        reason,
    })
}

/// Extracts the typed reason from an `io::Error` produced by `ensure_private_directory`.
pub fn unsafe_directory_reason(error: &io::Error) -> Option<UnsafeDirectoryReason> {
    error
        .get_ref()
        .and_then(|inner| inner.downcast_ref::<UnsafePrivateDirectoryError>())
        .map(|inner| inner.reason)
}

/// TS `ensurePrivateDirectory` with the process uid as the expected owner.
pub fn ensure_private_directory(path: &Path) -> io::Result<()> {
    ensure_private_directory_for(path, platform::current_uid())
}

/// TS `ensurePrivateDirectory({ currentUid })`: create 0700, refuse symlinks, non-dirs and
/// foreign owners, and chmod through a no-follow descriptor that must match the path.
pub fn ensure_private_directory_for(path: &Path, expected_uid: Option<u32>) -> io::Result<()> {
    create_dir_private(path)?;
    if cfg!(windows) {
        return Ok(());
    }
    let before = validate_private_directory(path, expected_uid)?;
    let file = platform::open_directory_following_nothing(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o700))?;
    }
    let after = validate_private_directory(path, expected_uid)?;
    let open = platform::directory_identity(&file)?;
    if before != after || before != open {
        return Err(unsafe_dir(path, UnsafeDirectoryReason::ChangedDuringChmod));
    }
    Ok(())
}

fn create_dir_private(path: &Path) -> io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    match builder.create(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(error),
    }
}

fn validate_private_directory(path: &Path, expected_uid: Option<u32>) -> io::Result<(u64, u64)> {
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        return Err(unsafe_dir(path, UnsafeDirectoryReason::Symlink));
    }
    if !metadata.is_dir() {
        return Err(unsafe_dir(path, UnsafeDirectoryReason::NotDirectory));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if expected_uid.is_some_and(|uid| uid != metadata.uid()) {
            return Err(unsafe_dir(path, UnsafeDirectoryReason::WrongOwner));
        }
        Ok((metadata.dev(), metadata.ino()))
    }
    #[cfg(not(unix))]
    {
        let _unused = expected_uid;
        Ok((0, 0))
    }
}

#[cfg(test)]
#[path = "ipc_protocol_tests.rs"]
mod tests;
