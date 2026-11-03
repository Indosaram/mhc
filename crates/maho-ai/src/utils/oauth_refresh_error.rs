//! Port of senpi packages/ai/src/utils/oauth-refresh-error.ts.
//!
//! senpi walks `error.cause` on untyped JS values and reads `name`/`code`/`status`. Rust errors
//! are typed, so the walk follows `std::error::Error::source()` and reads the same closed facts
//! from the concrete types that carry them: [`OAuthTokenEndpointError`] (`status`),
//! [`TransportCodeError`] (a platform/undici `code`), `std::io::Error` (its kind, mapped to the
//! matching POSIX code) and `reqwest::Error` (`is_timeout`). Messages and bodies are never read.

use crate::models::{ModelsError, ModelsErrorCode};
use std::error::Error as StdError;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct OAuthTokenEndpointError {
    pub message: String,
    pub status: u16,
}

impl OAuthTokenEndpointError {
    pub fn new(message: impl Into<String>, status: u16) -> Self {
        Self {
            message: message.into(),
            status,
        }
    }
}

/// An error carrying a transport `code` (`ECONNREFUSED`, `UND_ERR_SOCKET`, ...), the shape senpi
/// reads from Bun/Node/undici errors.
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct TransportCodeError {
    pub code: String,
    pub message: String,
    #[source]
    pub cause: Option<Box<dyn StdError + Send + Sync>>,
}

const TRANSPORT_CODES: &[&str] = &[
    "ConnectionRefused",
    "FailedToOpenSocket",
    "ConnectionClosed",
    "ECONNREFUSED",
    "ECONNRESET",
    "ENOTFOUND",
    "EAI_AGAIN",
    "ETIMEDOUT",
    "EPIPE",
    "UND_ERR_CONNECT_TIMEOUT",
    "UND_ERR_HEADERS_TIMEOUT",
    "UND_ERR_BODY_TIMEOUT",
    "UND_ERR_SOCKET",
];

/// Both cause walks are bounded: a refresh failure's cause chain is a few links deep.
const MAX_CAUSE_LINKS: usize = 16;

fn code_cause(code: &str) -> Option<String> {
    TRANSPORT_CODES.contains(&code).then(|| {
        if code == "ConnectionRefused" || code == "ECONNREFUSED" {
            "connection_refused".to_owned()
        } else {
            code.to_lowercase()
        }
    })
}

fn io_code(kind: std::io::ErrorKind) -> Option<&'static str> {
    use std::io::ErrorKind::*;
    Some(match kind {
        ConnectionRefused => "ECONNREFUSED",
        ConnectionReset | ConnectionAborted => "ECONNRESET",
        TimedOut => "ETIMEDOUT",
        BrokenPipe => "EPIPE",
        _ => return None,
    })
}

fn link_cause(error: &(dyn StdError + 'static)) -> Option<String> {
    if let Some(endpoint) = error.downcast_ref::<OAuthTokenEndpointError>() {
        let status = endpoint.status;
        return (status == 408 || status == 429 || (500..600).contains(&status))
            .then(|| format!("http_{status}"));
    }
    if let Some(coded) = error.downcast_ref::<TransportCodeError>() {
        return code_cause(&coded.code);
    }
    if let Some(io) = error.downcast_ref::<std::io::Error>() {
        return io_code(io.kind()).and_then(code_cause);
    }
    if let Some(http) = error.downcast_ref::<reqwest::Error>()
        && http.is_timeout()
    {
        return Some("timeout".to_owned());
    }
    None
}

/// Closed, log-safe facts only; never inspect the message or response body.
pub fn oauth_refresh_failure_cause(error: &(dyn StdError + 'static)) -> Option<String> {
    let mut current = Some(error);
    let mut links = 0;
    while let Some(error) = current {
        if links >= MAX_CAUSE_LINKS {
            break;
        }
        links += 1;
        if let Some(cause) = link_cause(error) {
            return Some(cause);
        }
        current = error.source();
    }
    None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OAuthRefreshFailureClass {
    Transient,
    Permanent,
}

pub fn classify_oauth_refresh_failure(
    error: &(dyn StdError + 'static),
) -> OAuthRefreshFailureClass {
    match oauth_refresh_failure_cause(error) {
        Some(_) => OAuthRefreshFailureClass::Transient,
        None => OAuthRefreshFailureClass::Permanent,
    }
}

/// `OAuthRefreshUnavailableError extends ModelsError`: code `oauth`, the refresh failure as cause.
#[derive(Debug, thiserror::Error)]
#[error("{models_error}")]
pub struct OAuthRefreshUnavailableError {
    pub models_error: ModelsError,
    #[source]
    pub cause: Option<Box<dyn StdError + Send + Sync>>,
}

impl OAuthRefreshUnavailableError {
    pub fn new(provider: &str, cause: Option<Box<dyn StdError + Send + Sync>>) -> Self {
        Self {
            models_error: ModelsError::new(
                ModelsErrorCode::OAuth,
                format!("OAuth refresh failed for {provider}"),
            ),
            cause,
        }
    }
}

pub fn is_oauth_refresh_unavailable_error(error: &(dyn StdError + 'static)) -> bool {
    let mut current = Some(error);
    let mut links = 0;
    while let Some(error) = current {
        if links >= MAX_CAUSE_LINKS {
            break;
        }
        links += 1;
        if error.is::<OAuthRefreshUnavailableError>() {
            return true;
        }
        current = error.source();
    }
    false
}

#[cfg(test)]
#[path = "oauth_refresh_error_tests.rs"]
mod tests;
