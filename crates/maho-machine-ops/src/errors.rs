//! Port of `packages/machine-ops/src/errors.ts`.

use serde::{Deserialize, Serialize};
use std::fmt;

/// Typed error code for machine operation failures.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpErrorCode {
    PolicyDenied,
    NotFound,
    LimitExceeded,
    InvalidPattern,
    InvalidArgument,
    PermissionDenied,
    Aborted,
    IoError,
}

impl OpErrorCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PolicyDenied => "policy_denied",
            Self::NotFound => "not_found",
            Self::LimitExceeded => "limit_exceeded",
            Self::InvalidPattern => "invalid_pattern",
            Self::InvalidArgument => "invalid_argument",
            Self::PermissionDenied => "permission_denied",
            Self::Aborted => "aborted",
            Self::IoError => "io_error",
        }
    }
}

impl fmt::Display for OpErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// An error raised by the operation layer itself; wrapped OS errors also preserve their fs errno
/// code when available so tools can display the exact errno string (`ENOENT`, `EISDIR`, etc.).
#[derive(Debug, Clone, thiserror::Error)]
#[error("{message}")]
pub struct MachineOpError {
    pub code: OpErrorCode,
    pub message: String,
    pub fs_code: Option<String>,
}

impl MachineOpError {
    pub fn new(code: OpErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            fs_code: None,
        }
    }

    pub fn with_fs_code(
        code: OpErrorCode,
        message: impl Into<String>,
        fs_code: impl Into<String>,
    ) -> Self {
        Self {
            code,
            message: message.into(),
            fs_code: Some(fs_code.into()),
        }
    }

    pub fn from_io(
        error: &std::io::Error,
        syscall: Option<&str>,
        path: Option<&std::path::Path>,
    ) -> Self {
        let (op_code, fs_code) = classify_io_error(error);
        let message = match (fs_code, syscall, path) {
            (Some(code), Some(syscall), Some(path)) => {
                format!("{code}: {error}, {syscall} '{}'", path.display())
            }
            (Some(code), None, Some(path)) => {
                format!("{code}: {error}, '{}'", path.display())
            }
            _ => error.to_string(),
        };
        Self {
            code: op_code,
            message,
            fs_code: fs_code.map(str::to_owned),
        }
    }
}

impl From<std::io::Error> for MachineOpError {
    fn from(error: std::io::Error) -> Self {
        Self::from_io(&error, None, None)
    }
}

pub fn classify_io_error(error: &std::io::Error) -> (OpErrorCode, Option<&'static str>) {
    use std::io::ErrorKind;
    // Check raw OS errors first where ErrorKind doesn't distinguish EPERM/ENOTDIR/ELOOP cleanly.
    #[cfg(unix)]
    if let Some(raw) = error.raw_os_error() {
        match raw {
            libc_enoent if libc_enoent == nix::errno::Errno::ENOENT as i32 => {
                return (OpErrorCode::NotFound, Some("ENOENT"));
            }
            libc_enotdir if libc_enotdir == nix::errno::Errno::ENOTDIR as i32 => {
                return (OpErrorCode::NotFound, Some("ENOTDIR"));
            }
            libc_eacces if libc_eacces == nix::errno::Errno::EACCES as i32 => {
                return (OpErrorCode::PermissionDenied, Some("EACCES"));
            }
            libc_eperm if libc_eperm == nix::errno::Errno::EPERM as i32 => {
                return (OpErrorCode::PermissionDenied, Some("EPERM"));
            }
            libc_eisdir if libc_eisdir == nix::errno::Errno::EISDIR as i32 => {
                return (OpErrorCode::IoError, Some("EISDIR"));
            }
            libc_eloop if libc_eloop == nix::errno::Errno::ELOOP as i32 => {
                return (OpErrorCode::IoError, Some("ELOOP"));
            }
            libc_erofs if libc_erofs == nix::errno::Errno::EROFS as i32 => {
                return (OpErrorCode::IoError, Some("EROFS"));
            }
            libc_enospc if libc_enospc == nix::errno::Errno::ENOSPC as i32 => {
                return (OpErrorCode::IoError, Some("ENOSPC"));
            }
            libc_ebusy if libc_ebusy == nix::errno::Errno::EBUSY as i32 => {
                return (OpErrorCode::IoError, Some("EBUSY"));
            }
            _ => {}
        }
    }
    match error.kind() {
        ErrorKind::NotFound => (OpErrorCode::NotFound, Some("ENOENT")),
        ErrorKind::NotADirectory => (OpErrorCode::NotFound, Some("ENOTDIR")),
        ErrorKind::PermissionDenied => (OpErrorCode::PermissionDenied, Some("EACCES")),
        ErrorKind::IsADirectory => (OpErrorCode::IoError, Some("EISDIR")),
        ErrorKind::AlreadyExists => (OpErrorCode::IoError, Some("EEXIST")),
        ErrorKind::StorageFull => (OpErrorCode::IoError, Some("ENOSPC")),
        ErrorKind::ReadOnlyFilesystem => (OpErrorCode::IoError, Some("EROFS")),
        ErrorKind::ResourceBusy => (OpErrorCode::IoError, Some("EBUSY")),
        _ => (OpErrorCode::IoError, None),
    }
}

/// Classify any `MachineOpError` so a caller sees its canonical `OpErrorCode`.
pub fn op_error_code(error: &MachineOpError) -> OpErrorCode {
    error.code
}
