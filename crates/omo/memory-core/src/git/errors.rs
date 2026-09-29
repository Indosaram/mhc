//! Typed Git errors for memory storage operations.

use std::fmt;

/// Typed Git error representing command failure, timeout, missing binary, or repository state conflict.
#[derive(Debug)]
pub enum GitError {
    Command {
        argv: Vec<String>,
        code: i32,
        stdout: String,
        stderr: String,
    },
    Timeout {
        argv: Vec<String>,
        timeout_ms: u64,
    },
    NotFound,
    DirtyRepo {
        porcelain: String,
        encoding_diagnostics: Vec<String>,
    },
    NoEffectiveChanges {
        paths: Vec<String>,
    },
    InvalidPath(String),
    PathState(String),
    LockExhausted(String),
    Io(std::io::Error),
    Other(String),
}

impl fmt::Display for GitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Command {
                argv,
                code,
                stdout,
                stderr,
            } => {
                let detail = if !stderr.trim().is_empty() {
                    stderr.trim()
                } else if !stdout.trim().is_empty() {
                    stdout.trim()
                } else {
                    &format!("exit code {code}")
                };
                write!(f, "git {} failed: {detail}", argv.join(" "))
            }
            Self::Timeout { argv, timeout_ms } => {
                write!(f, "git {} timed out after {timeout_ms}ms", argv.join(" "))
            }
            Self::NotFound => {
                write!(
                    f,
                    "Git is required for memory storage but was not found on PATH."
                )
            }
            Self::DirtyRepo {
                porcelain,
                encoding_diagnostics,
            } => {
                let encoding = if encoding_diagnostics.is_empty() {
                    String::new()
                } else {
                    format!(
                        " Dirty markdown encoding issue(s): {}.",
                        encoding_diagnostics.join("; ")
                    )
                };
                write!(
                    f,
                    "Memory repo has uncommitted changes. Commit, discard, or sync them before using memory tools.\n{}{encoding}",
                    porcelain.trim_end()
                )
            }
            Self::NoEffectiveChanges { paths } => {
                write!(
                    f,
                    "Memory write produced no effective changes for: {}",
                    paths.join(", ")
                )
            }
            Self::InvalidPath(msg) => write!(f, "{msg}"),
            Self::PathState(msg) => write!(f, "{msg}"),
            Self::LockExhausted(msg) => write!(f, "{msg}"),
            Self::Io(err) => write!(f, "git I/O error: {err}"),
            Self::Other(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for GitError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            _ => None,
        }
    }
}

impl From<std::io::Error> for GitError {
    fn from(err: std::io::Error) -> Self {
        Self::Io(err)
    }
}

/// Specialized error indicating a dirty git repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirtyRepoError {
    pub porcelain: String,
    pub encoding_diagnostics: Vec<String>,
}

impl fmt::Display for DirtyRepoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let encoding = if self.encoding_diagnostics.is_empty() {
            String::new()
        } else {
            format!(
                " Dirty markdown encoding issue(s): {}.",
                self.encoding_diagnostics.join("; ")
            )
        };
        write!(
            f,
            "Memory repo has uncommitted changes. Commit, discard, or sync them before using memory tools.\n{}{encoding}",
            self.porcelain.trim_end()
        )
    }
}

impl std::error::Error for DirtyRepoError {}

/// Specialized error indicating no changes were staged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NoEffectiveChangesError {
    pub paths: Vec<String>,
}

impl fmt::Display for NoEffectiveChangesError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Memory write produced no effective changes for: {}",
            self.paths.join(", ")
        )
    }
}

impl std::error::Error for NoEffectiveChangesError {}

/// Specialized error for git path state problems.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitPathStateError(pub String);

impl fmt::Display for GitPathStateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for GitPathStateError {}

impl From<GitPathStateError> for GitError {
    fn from(err: GitPathStateError) -> Self {
        Self::PathState(err.0)
    }
}
