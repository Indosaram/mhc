//! Port of senpi packages/coding-agent/src/core/session-cwd.ts.

use std::fmt;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionCwdIssue {
    pub session_file: Option<String>,
    pub session_cwd: String,
    pub fallback_cwd: String,
}

/// The slice of SessionManager this module reads.
pub trait SessionCwdSource {
    fn get_cwd(&self) -> &str;
    fn get_session_file(&self) -> Option<&str>;
}

impl SessionCwdSource for crate::session_manager::SessionManager {
    fn get_cwd(&self) -> &str {
        self.cwd()
    }
    fn get_session_file(&self) -> Option<&str> {
        self.session_file()
    }
}

pub fn get_missing_session_cwd_issue(
    session_manager: &dyn SessionCwdSource,
    fallback_cwd: &str,
) -> Option<SessionCwdIssue> {
    let session_file = session_manager.get_session_file()?;
    let session_cwd = session_manager.get_cwd();
    if session_cwd.is_empty() || Path::new(session_cwd).exists() {
        return None;
    }
    Some(SessionCwdIssue {
        session_file: Some(session_file.to_owned()),
        session_cwd: session_cwd.to_owned(),
        fallback_cwd: fallback_cwd.to_owned(),
    })
}

pub fn format_missing_session_cwd_error(issue: &SessionCwdIssue) -> String {
    let session_file = issue
        .session_file
        .as_ref()
        .map(|file| format!("\nSession file: {file}"))
        .unwrap_or_default();
    format!(
        "Stored session working directory does not exist: {}{session_file}\nCurrent working directory: {}",
        issue.session_cwd, issue.fallback_cwd
    )
}

pub fn format_missing_session_cwd_prompt(issue: &SessionCwdIssue) -> String {
    format!(
        "cwd from session file does not exist\n{}\n\ncontinue in current cwd\n{}",
        issue.session_cwd, issue.fallback_cwd
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissingSessionCwdError {
    pub issue: SessionCwdIssue,
}

impl fmt::Display for MissingSessionCwdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&format_missing_session_cwd_error(&self.issue))
    }
}

impl std::error::Error for MissingSessionCwdError {}

pub fn assert_session_cwd_exists(
    session_manager: &dyn SessionCwdSource,
    fallback_cwd: &str,
) -> Result<(), MissingSessionCwdError> {
    match get_missing_session_cwd_issue(session_manager, fallback_cwd) {
        Some(issue) => Err(MissingSessionCwdError { issue }),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Source {
        cwd: String,
        session_file: Option<String>,
    }

    impl SessionCwdSource for Source {
        fn get_cwd(&self) -> &str {
            &self.cwd
        }
        fn get_session_file(&self) -> Option<&str> {
            self.session_file.as_deref()
        }
    }

    #[test]
    fn an_existing_cwd_is_not_an_issue() {
        let dir = tempfile::tempdir().expect("tempdir");
        let source = Source { cwd: dir.path().to_string_lossy().into_owned(), session_file: Some("/s.jsonl".into()) };
        assert!(get_missing_session_cwd_issue(&source, "/fallback").is_none());
    }

    #[test]
    fn a_missing_session_file_is_not_an_issue() {
        let source = Source { cwd: "/definitely/missing".into(), session_file: None };
        assert!(get_missing_session_cwd_issue(&source, "/fallback").is_none());
    }

    #[test]
    fn a_missing_cwd_is_reported_with_the_fallback() {
        let source = Source { cwd: "/definitely/missing".into(), session_file: Some("/s.jsonl".into()) };
        let issue = get_missing_session_cwd_issue(&source, "/fallback").expect("issue");
        assert_eq!(issue.session_cwd, "/definitely/missing");
        assert_eq!(issue.fallback_cwd, "/fallback");
        assert!(format_missing_session_cwd_error(&issue).contains("Session file: /s.jsonl"));
        assert!(format_missing_session_cwd_prompt(&issue).contains("continue in current cwd"));
    }

    #[test]
    fn assert_returns_the_typed_error() {
        let source = Source { cwd: "/definitely/missing".into(), session_file: Some("/s.jsonl".into()) };
        assert!(assert_session_cwd_exists(&source, "/fallback").is_err());
    }
}
