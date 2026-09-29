use crate::lsp::types::FailedServerLookupResult;
use std::fmt;

/// The LSP error family. Each variant mirrors one TS error class; [`LspError::name`]
/// returns the TS class name so callers that matched on `error.name` keep working.
#[derive(Debug, Clone, PartialEq)]
pub enum LspError {
    ConnectionClosed {
        server_id: String,
        root: String,
        message: Option<String>,
    },
    ProcessExited {
        server_id: String,
        root: String,
        exit_code: Option<i32>,
        stderr_tail: Option<String>,
    },
    RequestTimeout {
        method: String,
        stderr_tail: Option<String>,
    },
    InvalidPath(String),
    ServerLookup {
        message: String,
        lookup: Option<FailedServerLookupResult>,
    },
    ServerInitializing {
        method: String,
        stderr_tail: Option<String>,
    },
    ProcessSpawn(String),
    /// A JSON-RPC error response (TS `Error` named `JsonRpcError(<code>)`).
    JsonRpc {
        code: Option<i64>,
        message: String,
    },
    /// TS `LspClientNotStartedError`.
    NotStarted {
        server_id: String,
        root: String,
    },
    /// Aborted via a cancellation signal (TS `DOMException("Aborted", "AbortError")`).
    Aborted,
    /// Any other error, carrying its message (TS plain `Error`).
    Other(String),
}

impl LspError {
    pub fn name(&self) -> std::borrow::Cow<'static, str> {
        let name = match self {
            Self::ConnectionClosed { .. } => "LspConnectionClosedError",
            Self::ProcessExited { .. } => "LspProcessExitedError",
            Self::RequestTimeout { .. } => "LspRequestTimeoutError",
            Self::InvalidPath(_) => "LspInvalidPathError",
            Self::ServerLookup { .. } => "LspServerLookupError",
            Self::ServerInitializing { .. } => "LspServerInitializingError",
            Self::ProcessSpawn(_) => "LspProcessSpawnError",
            Self::Aborted => "AbortError",
            Self::JsonRpc {
                code: Some(code), ..
            } => return format!("JsonRpcError({code})").into(),
            Self::JsonRpc { code: None, .. } | Self::Other(_) => "Error",
            Self::NotStarted { .. } => "LspClientNotStartedError",
        };
        name.into()
    }

    pub fn other(message: impl Into<String>) -> Self {
        Self::Other(message.into())
    }

    /// Converts a request timeout into the "still initializing" error (TS
    /// `new LspServerInitializingError(timeout)`). Other errors are returned unchanged.
    pub fn into_server_initializing(self) -> Self {
        match self {
            Self::RequestTimeout {
                method,
                stderr_tail,
            } => Self::ServerInitializing {
                method,
                stderr_tail,
            },
            other => other,
        }
    }

    pub fn message(&self) -> String {
        self.to_string()
    }
}

fn timeout_message(method: &str, stderr_tail: Option<&str>) -> String {
    let suffix = match stderr_tail {
        Some(tail) if !tail.is_empty() => format!("\nrecent stderr: {tail}"),
        _ => String::new(),
    };
    format!("LSP request timeout (method: {method}){suffix}")
}

impl fmt::Display for LspError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ConnectionClosed {
                server_id,
                root,
                message,
            } => match message {
                Some(message) => f.write_str(message),
                None => write!(f, "LSP connection closed for {server_id} at {root}"),
            },
            Self::ProcessExited {
                server_id,
                root,
                exit_code,
                stderr_tail,
            } => {
                let code = exit_code.map_or_else(|| "null".to_string(), |code| code.to_string());
                write!(
                    f,
                    "LSP server {server_id} at {root} exited with code {code}"
                )?;
                if let Some(tail) = stderr_tail.as_deref().filter(|tail| !tail.is_empty()) {
                    write!(f, "\nstderr tail: {tail}")?;
                }
                Ok(())
            }
            Self::RequestTimeout {
                method,
                stderr_tail,
            } => f.write_str(&timeout_message(method, stderr_tail.as_deref())),
            Self::InvalidPath(message) | Self::ProcessSpawn(message) | Self::Other(message) => {
                f.write_str(message)
            }
            Self::ServerLookup { message, .. } => f.write_str(message),
            Self::ServerInitializing {
                method,
                stderr_tail,
            } => write!(
                f,
                "LSP server is still initializing. Please retry in a few seconds. Original error: {}",
                timeout_message(method, stderr_tail.as_deref())
            ),
            Self::Aborted => f.write_str("Aborted"),
            Self::JsonRpc { message, .. } => f.write_str(message),
            Self::NotStarted { .. } => f.write_str("LSP client not started"),
        }
    }
}

impl std::error::Error for LspError {}

impl From<crate::request_context::LspRequestContextUnavailableError> for LspError {
    fn from(error: crate::request_context::LspRequestContextUnavailableError) -> Self {
        Self::Other(error.to_string())
    }
}

impl From<std::io::Error> for LspError {
    fn from(error: std::io::Error) -> Self {
        Self::Other(error.to_string())
    }
}

/// TS `isLspDeadConnectionError`.
pub fn is_lsp_dead_connection_error(error: &LspError) -> bool {
    matches!(
        error,
        LspError::ConnectionClosed { .. } | LspError::ProcessExited { .. }
    )
}
