//! Port of `daemon-request-error.ts`: typed daemon transport failures.

use std::fmt;

/// Subclass discriminator of TS `DaemonRequestError`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DaemonRequestErrorKind {
    /// Plain `DaemonRequestError`.
    Request,
    /// `DaemonAuthenticationRejectedError`.
    AuthenticationRejected,
    /// `DaemonRequestCancelledError`.
    Cancelled,
    /// `DaemonRequestTimedOutError` with its budget.
    TimedOut { timeout_ms: u64 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonRequestError {
    pub kind: DaemonRequestErrorKind,
    pub message: String,
    pub request_written: bool,
}

impl DaemonRequestError {
    pub fn new(message: impl Into<String>, request_written: bool) -> Self {
        Self {
            kind: DaemonRequestErrorKind::Request,
            message: message.into(),
            request_written,
        }
    }

    /// TS `DaemonAuthenticationRejectedError`.
    pub fn authentication_rejected() -> Self {
        Self {
            kind: DaemonRequestErrorKind::AuthenticationRejected,
            message: "daemon authentication failed before dispatch".to_string(),
            request_written: true,
        }
    }

    /// TS `DaemonRequestCancelledError`.
    pub fn cancelled(request_written: bool) -> Self {
        Self {
            kind: DaemonRequestErrorKind::Cancelled,
            message: "daemon request cancelled".to_string(),
            request_written,
        }
    }

    /// TS `DaemonRequestTimedOutError`.
    pub fn timed_out(request_written: bool, timeout_ms: u64) -> Self {
        Self {
            kind: DaemonRequestErrorKind::TimedOut { timeout_ms },
            message: "daemon request timed out".to_string(),
            request_written,
        }
    }

    /// TS `error.name`.
    pub fn name(&self) -> &'static str {
        match self.kind {
            DaemonRequestErrorKind::Request => "DaemonRequestError",
            DaemonRequestErrorKind::AuthenticationRejected => "DaemonAuthenticationRejectedError",
            DaemonRequestErrorKind::Cancelled => "DaemonRequestCancelledError",
            DaemonRequestErrorKind::TimedOut { .. } => "DaemonRequestTimedOutError",
        }
    }
}

impl fmt::Display for DaemonRequestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for DaemonRequestError {}

/// Anything a daemon call can fail with: a typed request error or an opaque cause
/// (TS rejects with `unknown`; non-`DaemonRequestError` causes are carried as text).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DaemonCallError {
    Request(DaemonRequestError),
    Other(String),
}

impl fmt::Display for DaemonCallError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Request(error) => error.fmt(formatter),
            Self::Other(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for DaemonCallError {}

impl From<DaemonRequestError> for DaemonCallError {
    fn from(error: DaemonRequestError) -> Self {
        Self::Request(error)
    }
}
