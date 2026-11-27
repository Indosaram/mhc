//! Port of senpi `packages/coding-agent/src/modes/rpc/project-expected-cwd.ts`.
//!
//! A project write may state the folder its caller means (`expectedCwd`). A client that reaches a
//! session by a key that can since name another session (a remote desktop, through a relay) states
//! it, so the write lands only in the project it means: when the session's working directory is
//! another folder, the write is refused and nothing is written. Absent, the write goes as it always
//! has.

use crate::rpc_types::RPC_ERROR_INVALID_ARGUMENT;
use serde_json::Value;

/// A project write stated an `expectedCwd` that differs from the session's working directory.
pub const RPC_ERROR_PROJECT_CWD_MISMATCH: &str = "project_cwd_mismatch";

/// The project writes that may state the folder they mean.
const PROJECT_WRITES: [&str; 5] = [
    "set_project_trust",
    "set_project_setting",
    "write_project_file",
    "set_project_mcp_servers",
    "write_project_skill_file",
];

/// A folder path for comparison.
///
/// Trailing separators of the path's own style are dropped (only `/` for a POSIX path; `\\` or
/// `/` for a drive or UNC path), but a bare POSIX or drive root keeps one: `"/"` stays `"/"`, and
/// a bare drive root becomes `"X:\\"` (`"C:"`, `"C:/"` and `"c:\\"` all become `"C:\\"`). A Windows
/// drive letter is compared without case (`c:` and `C:` are one drive). Nothing else is folded.
pub fn comparable_folder(path: &str) -> String {
    if path.starts_with('/') {
        let stripped = path.trim_end_matches('/');
        return if stripped.is_empty() {
            "/".to_owned()
        } else {
            stripped.to_owned()
        };
    }
    let bytes = path.as_bytes();
    let has_drive = bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':';
    let folded = if has_drive {
        let mut out = String::with_capacity(path.len());
        out.push(bytes[0].to_ascii_uppercase() as char);
        out.push_str(&path[1..]);
        out
    } else {
        path.to_owned()
    };
    let stripped = folded.trim_end_matches(['/', '\\']);
    if has_drive && stripped.len() == 2 {
        format!("{stripped}\\")
    } else {
        stripped.to_owned()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CwdMismatch {
    pub message: &'static str,
    pub error_code: &'static str,
}

/// The refusal of a project write whose stated folder is not the session's, or whose `expectedCwd`
/// is present but not a string, or `None` when it may go.
pub fn project_cwd_mismatch(
    command_type: &str,
    expected_cwd: Option<&Value>,
    session_cwd: &str,
) -> Option<CwdMismatch> {
    if !PROJECT_WRITES.contains(&command_type) {
        return None;
    }
    let expected = expected_cwd?;
    let Some(expected_str) = expected.as_str() else {
        return Some(CwdMismatch {
            message: "expectedCwd must be a string: the folder this write is for; nothing was written.",
            error_code: RPC_ERROR_INVALID_ARGUMENT,
        });
    };
    if comparable_folder(expected_str) == comparable_folder(session_cwd) {
        None
    } else {
        Some(CwdMismatch {
            message: "This session is in another folder than the one this write is for; nothing was written.",
            error_code: RPC_ERROR_PROJECT_CWD_MISMATCH,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn comparable_folder_normalizes_posix_and_windows_roots() {
        assert_eq!(comparable_folder("/"), "/");
        assert_eq!(comparable_folder("///"), "/");
        assert_eq!(comparable_folder("/w/app/"), "/w/app");
        assert_eq!(comparable_folder("/w/app///"), "/w/app");
        assert_eq!(comparable_folder("/w/app "), "/w/app ");
        assert_eq!(comparable_folder("/w/app\\"), "/w/app\\");
        assert_eq!(comparable_folder("c:"), "C:\\");
        assert_eq!(comparable_folder("C:/"), "C:\\");
        assert_eq!(comparable_folder("c:\\\\"), "C:\\");
        assert_eq!(comparable_folder("d:\\repo\\"), "D:\\repo");
        assert_eq!(
            comparable_folder("\\\\server\\share\\"),
            "\\\\server\\share"
        );
    }

    #[test]
    fn project_cwd_mismatch_checks_only_project_writes() {
        assert_eq!(
            project_cwd_mismatch("prompt", Some(&json!("/other")), "/w/app"),
            None
        );
        assert_eq!(
            project_cwd_mismatch("write_project_file", None, "/w/app"),
            None
        );
        assert_eq!(
            project_cwd_mismatch("write_project_file", Some(&json!("/w/app/")), "/w/app"),
            None
        );
        let invalid = project_cwd_mismatch("set_project_trust", Some(&json!(123)), "/w/app")
            .expect("invalid expectedCwd");
        assert_eq!(invalid.error_code, RPC_ERROR_INVALID_ARGUMENT);
        let mismatch =
            project_cwd_mismatch("set_project_mcp_servers", Some(&json!("/other")), "/w/app")
                .expect("mismatch");
        assert_eq!(mismatch.error_code, RPC_ERROR_PROJECT_CWD_MISMATCH);
    }
}
