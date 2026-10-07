//! Storage layout constants and path builders for memory identities.
//!
//! Layout: `<memory-root>/agents/<safe-id>/{repo,runtime/...}` where
//! `memory-root` is the `OMO_MEMORY_HOME` override or `~/.omo/memory`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::support::paths::resolve_from;

pub const MEMORY_ROOT_ENV_VAR: &str = "MAHO_MEMORY_HOME";
pub const AGENTS_DIRNAME: &str = "agents";
pub const REPO_DIRNAME: &str = "repo";
pub const RUNTIME_DIRNAME: &str = "runtime";

pub const RUNTIME_SUBDIRNAMES: [&str; 12] = [
    "locks",
    "transcripts",
    "reflection",
    "reflection-sessions",
    "worktrees",
    "viewers",
    "push-queue",
    "facts-queue",
    "facts",
    "notices",
    "tool-receipts",
    "recall",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryIdentityPaths {
    pub root: PathBuf,
    pub repo: PathBuf,
    pub runtime: PathBuf,
    pub locks: PathBuf,
    pub transcripts: PathBuf,
    pub reflection: PathBuf,
    pub reflection_sessions: PathBuf,
    pub worktrees: PathBuf,
    pub viewers: PathBuf,
    pub push_queue: PathBuf,
    pub facts_queue: PathBuf,
    pub facts: PathBuf,
    pub notices: PathBuf,
    pub tool_receipts: PathBuf,
    /// Recall runtime tree: per-session surfaced-path ledgers and pending gate nudges.
    pub recall: PathBuf,
    pub recall_ledger: PathBuf,
    pub recall_pending: PathBuf,
}

/// `~/.omo/memory` (Windows: `%USERPROFILE%\.omo\memory`).
pub fn default_memory_root() -> PathBuf {
    home_directory().join(".maho").join("memory")
}

/// `OMO_MEMORY_HOME` when set and non-blank, resolved against `cwd`; otherwise
/// the default root.
pub fn resolve_memory_root(env: &BTreeMap<String, String>, cwd: &Path) -> PathBuf {
    match env.get(MEMORY_ROOT_ENV_VAR) {
        Some(override_path) if !override_path.trim().is_empty() => {
            resolve_from(cwd, Path::new(override_path.trim()))
        }
        _ => default_memory_root(),
    }
}

pub fn build_identity_paths(memory_root: &Path, id: &str) -> MemoryIdentityPaths {
    let root = memory_root.join(AGENTS_DIRNAME).join(id);
    let runtime = root.join(RUNTIME_DIRNAME);
    MemoryIdentityPaths {
        repo: root.join(REPO_DIRNAME),
        locks: runtime.join("locks"),
        transcripts: runtime.join("transcripts"),
        reflection: runtime.join("reflection"),
        reflection_sessions: runtime.join("reflection-sessions"),
        worktrees: runtime.join("worktrees"),
        viewers: runtime.join("viewers"),
        push_queue: runtime.join("push-queue"),
        facts_queue: runtime.join("facts-queue"),
        facts: runtime.join("facts"),
        notices: runtime.join("notices"),
        tool_receipts: runtime.join("tool-receipts"),
        recall: runtime.join("recall"),
        recall_ledger: runtime.join("recall").join("ledger"),
        recall_pending: runtime.join("recall").join("pending"),
        runtime,
        root,
    }
}

/// The process home directory, matching `os.homedir()` as closely as the
/// standard library allows.
pub fn home_directory() -> PathBuf {
    for key in ["HOME", "USERPROFILE"] {
        if let Ok(value) = std::env::var(key)
            && !value.trim().is_empty()
        {
            return PathBuf::from(value);
        }
    }
    if let (Ok(drive), Ok(path)) = (std::env::var("HOMEDRIVE"), std::env::var("HOMEPATH"))
        && !path.trim().is_empty()
    {
        return PathBuf::from(format!("{drive}{path}"));
    }
    PathBuf::from(".")
}

#[cfg(test)]
#[path = "layout_tests.rs"]
mod tests;
