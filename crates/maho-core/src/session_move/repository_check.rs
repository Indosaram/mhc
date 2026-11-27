//! Port of senpi packages/coding-agent/src/core/session-move/repository-check.ts.
//!
//! The one comparison the target's `prepare` and the `import` both make (senpi#6435): is this
//! folder a checkout of the repository the session recorded?

use std::future::Future;
use std::path::Path;
use std::pin::Pin;

use crate::paths::{PathInputOptions, resolve_path};
use crate::repository_identity::{compare_repository_identities, read_repository_identity};
use crate::session_move::bundle::RepositoryIdentity;

pub type AsyncRepositoryReader<'a> = &'a (
        dyn Fn(&str) -> Pin<Box<dyn Future<Output = Option<RepositoryIdentity>> + Send>>
            + Send
            + Sync
    );

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoveRefusedError {
    pub target_cwd: String,
    pub source_cwd: Option<String>,
}

impl MoveRefusedError {
    pub const CODE: &'static str = "move_repository_mismatch";

    pub fn new(target_cwd: impl Into<String>, source_cwd: Option<String>) -> Self {
        Self {
            target_cwd: target_cwd.into(),
            source_cwd,
        }
    }
}

impl std::fmt::Display for MoveRefusedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.source_cwd {
            Some(src) => write!(
                f,
                "{} is not the same repository the session was recorded in ({src}). Pick a checkout of the same repository, or copy the session into this folder instead.",
                self.target_cwd
            ),
            None => write!(
                f,
                "{} is not the same repository the session was recorded in. Pick a checkout of the same repository, or copy the session into this folder instead.",
                self.target_cwd
            ),
        }
    }
}

impl std::error::Error for MoveRefusedError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoveTargetMissingError {
    pub target_cwd: String,
}

impl MoveTargetMissingError {
    pub const CODE: &'static str = "workspace_missing";

    pub fn new(target_cwd: impl Into<String>) -> Self {
        Self {
            target_cwd: target_cwd.into(),
        }
    }
}

impl std::fmt::Display for MoveTargetMissingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} does not exist on this device. Clone the repository there, or pick a folder that holds it, then send the session again.",
            self.target_cwd
        )
    }
}

impl std::error::Error for MoveTargetMissingError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckoutVerdict {
    Same,
    Missing,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckoutRefusal {
    Mismatch(MoveRefusedError),
    Missing(MoveTargetMissingError),
}

impl CheckoutRefusal {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Mismatch(_) => MoveRefusedError::CODE,
            Self::Missing(_) => MoveTargetMissingError::CODE,
        }
    }
}

impl std::fmt::Display for CheckoutRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Mismatch(e) => write!(f, "{}: {e}", MoveRefusedError::CODE),
            Self::Missing(e) => write!(f, "{}: {e}", MoveTargetMissingError::CODE),
        }
    }
}

impl std::error::Error for CheckoutRefusal {}

pub async fn checkout_verdict(
    cwd: &str,
    repository: Option<&RepositoryIdentity>,
    read: Option<AsyncRepositoryReader<'_>>,
) -> CheckoutVerdict {
    let base = std::env::current_dir().unwrap_or_default();
    let base_str = base.to_string_lossy();
    let folder = resolve_path(cwd, &base_str, &PathInputOptions::default());
    let live = match read {
        Some(f) => f(&folder).await,
        None => read_repository_identity(&folder).await,
    };
    if compare_repository_identities(repository, live.as_ref()) == "same" {
        return CheckoutVerdict::Same;
    }
    if tokio::fs::try_exists(Path::new(&folder))
        .await
        .unwrap_or(false)
    {
        CheckoutVerdict::Other
    } else {
        CheckoutVerdict::Missing
    }
}

pub fn checkout_refusal(
    verdict: CheckoutVerdict,
    target_cwd: impl Into<String>,
    source_cwd: Option<String>,
) -> CheckoutRefusal {
    match verdict {
        CheckoutVerdict::Missing => {
            CheckoutRefusal::Missing(MoveTargetMissingError::new(target_cwd))
        }
        CheckoutVerdict::Other | CheckoutVerdict::Same => {
            CheckoutRefusal::Mismatch(MoveRefusedError::new(target_cwd, source_cwd))
        }
    }
}
