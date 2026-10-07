use std::path::{Path, PathBuf};

/// Sidecar naming the backend that built a sandbox, so a later sweep can route teardown through it.
pub const BACKEND_FILE: &str = ".omo-isolation-backend.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BackendKind {
    Apfs,
    Btrfs,
    Zfs,
    Reflink,
    Overlayfs,
    BlockClone,
    Rcopy,
}

impl BackendKind {
    pub const ALL: [BackendKind; 7] = [
        BackendKind::Apfs,
        BackendKind::Btrfs,
        BackendKind::Zfs,
        BackendKind::Reflink,
        BackendKind::Overlayfs,
        BackendKind::BlockClone,
        BackendKind::Rcopy,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            BackendKind::Apfs => "apfs",
            BackendKind::Btrfs => "btrfs",
            BackendKind::Zfs => "zfs",
            BackendKind::Reflink => "reflink",
            BackendKind::Overlayfs => "overlayfs",
            BackendKind::BlockClone => "block-clone",
            BackendKind::Rcopy => "rcopy",
        }
    }

    pub fn parse(value: &str) -> Option<BackendKind> {
        BackendKind::ALL.into_iter().find(|kind| kind.as_str() == value)
    }
}

pub fn is_backend_kind(value: &str) -> bool {
    BackendKind::parse(value).is_some()
}

pub type Result<T> = std::result::Result<T, IsolationError>;

#[derive(Debug, thiserror::Error)]
pub enum IsolationError {
    #[error("{message}")]
    Unavailable { message: String },
    #[error("{message}")]
    Exists { message: String },
    #[error("{message}")]
    BaselineTooLarge {
        repo_root: PathBuf,
        content_bytes: Option<u64>,
        budget_bytes: u64,
        message: String,
    },
    #[error("{message}")]
    Git {
        args: Vec<String>,
        cwd: PathBuf,
        code: i32,
        stderr: String,
        message: String,
    },
    #[error("{message}")]
    GitTimeout {
        args: Vec<String>,
        cwd: PathBuf,
        timeout_ms: u64,
        message: String,
    },
    #[error("{message}")]
    CommitReplay {
        commit: String,
        branch_name: String,
        cause: String,
        message: String,
    },
    #[error("{message}")]
    Clone { errno: i32, message: String },
    #[error("{message}")]
    Io {
        kind: std::io::ErrorKind,
        message: String,
    },
    #[error("{message}")]
    Other { message: String },
}

impl IsolationError {
    pub fn unavailable(message: impl Into<String>) -> Self {
        IsolationError::Unavailable {
            message: message.into(),
        }
    }

    pub fn exists(message: impl Into<String>) -> Self {
        IsolationError::Exists {
            message: message.into(),
        }
    }

    pub fn other(message: impl Into<String>) -> Self {
        IsolationError::Other {
            message: message.into(),
        }
    }

    pub fn is_unavailable(&self) -> bool {
        matches!(self, IsolationError::Unavailable { .. })
    }

    pub fn is_not_found(&self) -> bool {
        matches!(
            self,
            IsolationError::Io { kind, .. } if *kind == std::io::ErrorKind::NotFound
        )
    }

    pub fn code(&self) -> Option<&'static str> {
        match self {
            IsolationError::Unavailable { .. } => Some("isolation_unavailable"),
            IsolationError::Exists { .. } => Some("isolation_exists"),
            _ => None,
        }
    }
}

impl From<std::io::Error> for IsolationError {
    fn from(error: std::io::Error) -> Self {
        IsolationError::Io {
            kind: error.kind(),
            message: error.to_string(),
        }
    }
}

pub fn error_text(error: &IsolationError) -> String {
    match error {
        IsolationError::Git { stderr, .. } => stderr.clone(),
        other => other.to_string(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeResult {
    pub available: bool,
    pub reason: Option<String>,
}

impl ProbeResult {
    pub fn available() -> Self {
        ProbeResult {
            available: true,
            reason: None,
        }
    }

    pub fn unavailable(reason: impl Into<String>) -> Self {
        ProbeResult {
            available: false,
            reason: Some(reason.into()),
        }
    }
}

#[derive(Debug, Clone)]
pub struct IsolationContext {
    pub id: String,
    pub base_dir: PathBuf,
    pub cross_device: bool,
    pub max_copy_bytes: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct StartDetail {
    pub strategy_detail: String,
}

pub trait IsolationBackend: Send + Sync {
    fn kind(&self) -> BackendKind;
    fn clones_tree(&self) -> bool;
    fn probe(&self, repo_root: &Path, ctx: Option<&IsolationContext>) -> Result<ProbeResult>;
    fn start(
        &self,
        lower: &Path,
        merged: &Path,
        ctx: &IsolationContext,
    ) -> Result<Option<StartDetail>>;
    fn stop(&self, merged: &Path) -> Result<()>;

    /// Relocate the sandbox parent, including any mounted merged tree.
    fn relocate(&self, from: &Path, to: &Path) -> Result<()> {
        std::fs::rename(from, to)?;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidates {
    pub candidates: Vec<BackendKind>,
    pub reason: Option<String>,
}

pub fn resolve_candidates(platform: &str, preferred: Option<BackendKind>) -> Candidates {
    let mut candidates: Vec<BackendKind> = match platform {
        "darwin" => vec![BackendKind::Apfs, BackendKind::Zfs, BackendKind::Rcopy],
        "linux" => vec![
            BackendKind::Btrfs,
            BackendKind::Zfs,
            BackendKind::Reflink,
            BackendKind::Overlayfs,
            BackendKind::Rcopy,
        ],
        "win32" => vec![BackendKind::BlockClone, BackendKind::Rcopy],
        _ => vec![BackendKind::Rcopy],
    };
    if let Some(preferred) = preferred {
        candidates.retain(|kind| *kind != preferred);
        candidates.insert(0, preferred);
    }
    Candidates {
        candidates,
        reason: None,
    }
}

pub fn platform_name() -> &'static str {
    if cfg!(target_os = "macos") {
        "darwin"
    } else if cfg!(target_os = "windows") {
        "win32"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else {
        std::env::consts::OS
    }
}
