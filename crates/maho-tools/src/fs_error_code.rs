//! Port of `packages/coding-agent/src/core/tools/fs-error-code.ts`.

use crate::definition::ToolError;
use maho_machine_ops::{MachineOpError, OpErrorCode};

fn is_fs_code(code: &str) -> bool {
    code == "ABORT_ERR"
        || (code.starts_with('E')
            && code.len() > 1
            && code[1..]
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit()))
}

pub fn op_code_to_fs_code(code: OpErrorCode) -> &'static str {
    match code {
        OpErrorCode::NotFound => "ENOENT",
        OpErrorCode::PermissionDenied => "EACCES",
        OpErrorCode::Aborted => "ABORT_ERR",
        OpErrorCode::PolicyDenied => "policy_denied",
        OpErrorCode::LimitExceeded => "limit_exceeded",
        OpErrorCode::InvalidPattern => "invalid_pattern",
        OpErrorCode::InvalidArgument => "invalid_argument",
        OpErrorCode::IoError => "io_error",
    }
}

/// Extract the OS error code string from a [`MachineOpError`], preferring its preserved `fs_code`
/// and mapping operation error codes (`NotFound -> ENOENT`, `PermissionDenied -> EACCES`, `Aborted -> ABORT_ERR`).
pub fn machine_error_fs_code(error: &MachineOpError) -> &'static str {
    if let Some(fs_code) = &error.fs_code
        && is_fs_code(fs_code)
    {
        return match fs_code.as_str() {
            "ENOENT" => "ENOENT",
            "ENOTDIR" => "ENOTDIR",
            "EACCES" => "EACCES",
            "EPERM" => "EPERM",
            "EISDIR" => "EISDIR",
            "ELOOP" => "ELOOP",
            "EEXIST" => "EEXIST",
            "ENOSPC" => "ENOSPC",
            "EROFS" => "EROFS",
            "EMFILE" => "EMFILE",
            "ENAMETOOLONG" => "ENAMETOOLONG",
            "EBUSY" => "EBUSY",
            "ABORT_ERR" => "ABORT_ERR",
            _ => op_code_to_fs_code(error.code),
        };
    }
    op_code_to_fs_code(error.code)
}

/// Extract the OS error code string from a [`MachineOpError`] preserving arbitrary `E*` `fs_code`.
pub fn fs_error_code(error: &MachineOpError) -> String {
    if let Some(fs_code) = &error.fs_code
        && is_fs_code(fs_code)
    {
        return fs_code.clone();
    }
    op_code_to_fs_code(error.code).to_owned()
}

pub fn io_error_fs_code(error: &std::io::Error) -> &'static str {
    let op = MachineOpError::from_io(error, None, None);
    machine_error_fs_code(&op)
}

pub fn tool_error_from_machine(error: MachineOpError) -> ToolError {
    match error.code {
        OpErrorCode::Aborted => ToolError::Aborted,
        _ => ToolError::Message(error.message),
    }
}
