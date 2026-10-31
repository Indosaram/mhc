//! Port of senpi `packages/coding-agent/src/core/session-hold-errors.ts`.
//!
//! `SessionHolder` is `crate::session_holder_records::SessionHolder`; `SessionHolderEndpoint` lives in
//! maho-rpc (`session_hold_errors.rs`), which owns the host identity.

pub use crate::session_holder_records::SessionHolder;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct SessionHeldError {
    pub session_file: String,
    pub holders: Vec<SessionHolder>,
    message: String,
}

impl SessionHeldError {
    pub const CODE: &'static str = "ESESSIONHELD";

    pub fn new(session_file: impl Into<String>, holders: Vec<SessionHolder>) -> Self {
        let who = holders
            .iter()
            .map(
                |holder| match holder.cwd.as_deref().filter(|cwd| !cwd.is_empty()) {
                    Some(cwd) => format!("pid {} in {cwd}", holder.pid),
                    None => format!("pid {}", holder.pid),
                },
            )
            .collect::<Vec<_>>()
            .join("; ");
        let message = format!(
            "This session is open in another {} process ({who}). Quit it there, then try again.",
            crate::config::app_name()
        );
        Self {
            session_file: session_file.into(),
            holders,
            message,
        }
    }

    pub fn code(&self) -> &'static str {
        Self::CODE
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct SessionMovingError {
    pub mover_pid: Option<u32>,
    message: String,
}

impl SessionMovingError {
    pub const CODE: &'static str = "ESESSIONMOVING";

    pub fn new(session_file: &str, mover_pid: Option<u32>) -> Self {
        let who = match mover_pid {
            None => "another process".to_owned(),
            Some(pid) => format!("another process (pid {pid})"),
        };
        Self {
            mover_pid,
            message: format!(
                "Session {session_file} is being moved by {who}. Try again in a moment."
            ),
        }
    }

    pub fn code(&self) -> &'static str {
        Self::CODE
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "Session {session_file} no longer exists: another process moved it. Resume it from its new location."
)]
pub struct SessionMovedError {
    pub session_file: String,
}

impl SessionMovedError {
    pub const CODE: &'static str = "ESESSIONMOVED";

    pub fn new(session_file: impl Into<String>) -> Self {
        Self {
            session_file: session_file.into(),
        }
    }

    pub fn code(&self) -> &'static str {
        Self::CODE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn held_error_lists_every_holder() {
        let holders = vec![
            SessionHolder {
                pid: 1,
                cwd: Some("/a".into()),
                host_socket: None,
                host_instance: None,
            },
            SessionHolder {
                pid: 2,
                cwd: None,
                host_socket: None,
                host_instance: None,
            },
        ];
        let error = SessionHeldError::new("/s.jsonl", holders);
        assert!(
            error
                .to_string()
                .ends_with("process (pid 1 in /a; pid 2). Quit it there, then try again.")
        );
        assert!(
            error
                .to_string()
                .starts_with("This session is open in another ")
        );
        assert_eq!(error.code(), "ESESSIONHELD");
    }

    #[test]
    fn moving_and_moved_messages_match_the_pinned_source() {
        assert_eq!(
            SessionMovingError::new("/s", Some(7)).to_string(),
            "Session /s is being moved by another process (pid 7). Try again in a moment."
        );
        assert_eq!(
            SessionMovingError::new("/s", None).to_string(),
            "Session /s is being moved by another process. Try again in a moment."
        );
        assert_eq!(
            SessionMovedError::new("/s").to_string(),
            "Session /s no longer exists: another process moved it. Resume it from its new location."
        );
    }
}
