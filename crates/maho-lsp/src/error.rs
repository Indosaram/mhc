use std::fmt;

/// Failures of LSP operations. Messages follow the upstream wording.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LspError {
    /// No server configured / server not installed (formatted lookup message).
    ServerLookup(String),
    InvalidPath(String),
    ProcessExited {
        server_id: String,
        root: String,
        exit_code: Option<i32>,
        stderr_tail: Option<String>,
    },
    ConnectionClosed {
        server_id: String,
        root: String,
        message: Option<String>,
    },
    RequestTimeout {
        method: String,
        stderr_tail: Option<String>,
    },
    /// A request timed out while the server was still initializing.
    Initializing(String),
    Spawn(String),
    /// JSON-RPC error response.
    Rpc {
        code: i64,
        message: String,
    },
    Aborted,
    Other(String),
}

impl LspError {
    /// The server process died or the connection closed: a read-only call may retry once.
    pub fn is_dead_connection(&self) -> bool {
        matches!(
            self,
            Self::ProcessExited { .. } | Self::ConnectionClosed { .. }
        )
    }
}

impl fmt::Display for LspError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ServerLookup(message)
            | Self::InvalidPath(message)
            | Self::Spawn(message)
            | Self::Other(message) => f.write_str(message),
            Self::ProcessExited {
                server_id,
                root,
                exit_code,
                stderr_tail,
            } => {
                let code = exit_code.map_or_else(|| "null".to_owned(), |code| code.to_string());
                write!(
                    f,
                    "LSP server {server_id} at {root} exited with code {code}"
                )?;
                if let Some(tail) = stderr_tail.as_deref().filter(|tail| !tail.is_empty()) {
                    write!(f, "\nstderr tail: {tail}")?;
                }
                Ok(())
            }
            Self::ConnectionClosed {
                server_id,
                root,
                message,
            } => match message {
                Some(message) => f.write_str(message),
                None => write!(f, "LSP connection closed for {server_id} at {root}"),
            },
            Self::RequestTimeout {
                method,
                stderr_tail,
            } => {
                write!(f, "LSP request timeout (method: {method})")?;
                if let Some(tail) = stderr_tail.as_deref().filter(|tail| !tail.is_empty()) {
                    write!(f, "\nrecent stderr: {tail}")?;
                }
                Ok(())
            }
            Self::Initializing(original) => write!(
                f,
                "LSP server is still initializing. Please retry in a few seconds. Original error: {original}"
            ),
            Self::Rpc { code, message } => write!(f, "LSP error {code}: {message}"),
            Self::Aborted => f.write_str("Aborted"),
        }
    }
}

impl std::error::Error for LspError {}

const RUST_SRC_REPAIR: &str = "rust-analyzer exited while loading Rust standard library sources.\n\nRepair rust-src for the active toolchain:\n  rustup component remove rust-src\n  rustup component add rust-src";

/// Repair guidance for a known startup failure (rust-analyzer without a usable rust-src).
pub fn known_startup_failure(error: &LspError) -> Option<String> {
    let LspError::ProcessExited {
        server_id,
        stderr_tail,
        ..
    } = error
    else {
        return None;
    };
    if server_id != "rust" {
        return None;
    }
    let details = stderr_tail.clone().unwrap_or_else(|| error.to_string());
    let lower = details.to_lowercase();
    let rust_src = lower.contains("rust-src")
        && [
            "failed to install component",
            "detected conflict",
            "can't load standard library",
            "try installing",
            "sysroot",
        ]
        .iter()
        .any(|needle| lower.contains(needle));
    rust_src.then(|| format!("{RUST_SRC_REPAIR}\n\nOriginal stderr tail:\n{details}"))
}

/// The user-facing message for "the server is missing", or `None` for other failures.
pub fn missing_dependency_message(error: &LspError) -> Option<String> {
    if let Some(message) = known_startup_failure(error) {
        return Some(message);
    }
    let message = error.to_string();
    (message.contains("NOT INSTALLED") || message.contains("No LSP server configured"))
        .then_some(message)
}
