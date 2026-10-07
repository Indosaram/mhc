//! Git repository types and parameter contracts.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::errors::GitError;
use super::exec::GitExec;

/// Author attribution for Git commits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitCommitAuthor {
    pub agent_id: String,
    pub author_name: String,
    pub author_email: Option<String>,
}

/// Initial file content seeded during repository creation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitSeedFile {
    pub relative_path: String,
    pub content: String,
}

/// Result of a successful Git commit operation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitCommitResult {
    pub committed: bool,
    pub sha: String,
}

/// Parsed commit entry from Git log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryCommit {
    pub sha: String,
    pub subject: String,
    pub body: String,
    pub author_name: String,
    pub author_email: String,
    pub committed_at: String,
    pub trailers: BTreeMap<String, String>,
    pub paths: Option<Vec<String>>,
}

/// Filter and pagination options for Git log queries.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GitLogOptions {
    pub range: Option<String>,
    pub paths: Option<Vec<String>>,
    pub limit: Option<usize>,
    pub include_paths: bool,
}

/// One `git ls-tree -l` entry with its blob byte size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitTreeSizedEntry {
    pub path: String,
    pub bytes: u64,
}

/// One `git ls-tree` blob entry with its object id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitTreeBlobEntry {
    pub path: String,
    pub oid: String,
}

/// Hook installer callback type executed on repository setup and write boundaries.
pub type GitHookInstaller = Arc<dyn Fn(&Path) -> Result<(), GitError> + Send + Sync>;

/// Configuration options for initializing a repository.
#[derive(Default, Clone)]
pub struct InitializeGitRepoOptions {
    pub author_name: Option<String>,
    pub seed_files: Vec<GitSeedFile>,
    pub install_hooks: Option<GitHookInstaller>,
}

/// Configuration options for constructing a GitMemoryRepo instance.
#[derive(Clone)]
pub struct GitMemoryRepoOptions {
    pub dir: PathBuf,
    pub agent_id: String,
    pub exec: Option<Arc<dyn GitExec>>,
    pub install_hooks: Option<GitHookInstaller>,
}

impl GitMemoryRepoOptions {
    pub fn new(dir: impl Into<PathBuf>, agent_id: impl Into<String>) -> Self {
        Self {
            dir: dir.into(),
            agent_id: agent_id.into(),
            exec: None,
            install_hooks: None,
        }
    }
}

impl<P: Into<PathBuf>, S: Into<String>> From<(P, S)> for GitMemoryRepoOptions {
    fn from((dir, agent_id): (P, S)) -> Self {
        Self::new(dir, agent_id)
    }
}

/// Options controlling branch merge operations.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GitMergeOptions {
    pub no_ff: Option<bool>,
    pub message: Option<String>,
}
