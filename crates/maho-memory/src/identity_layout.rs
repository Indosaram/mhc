//! On-disk layout of a memory identity, and where its content lives.
//!
//! Spec: `docs/cleanroom/specs/memory-identity-layout.md`. Two trees sit under one identity directory: `repo/` is
//! the git-backed content the agent reads and writes, and `runtime/` is machine state (locks, transcripts,
//! queues). Only `repo/` is memory; everything under `runtime/` is disposable and must never be imported as
//! notes.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const MEMORY_ROOT_ENV_VAR: &str = "MAHO_MEMORY_HOME";
pub const AGENTS_DIRNAME: &str = "agents";
pub const REPO_DIRNAME: &str = "repo";
pub const RUNTIME_DIRNAME: &str = "runtime";

pub const RUNTIME_SUBDIRNAMES: [&str; 14] = [
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
    "memory-usage",
    "memory-maintenance",
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
    pub recall: PathBuf,
    pub recall_ledger: PathBuf,
    pub recall_pending: PathBuf,
    pub memory_usage: PathBuf,
    pub memory_maintenance: PathBuf,
}

pub fn default_memory_root() -> PathBuf {
    home_directory().join(".omo").join("memory")
}

/// Resolve the memory root: the override when it is set and non-blank, resolved against `cwd`.
///
/// A blank override means "unset" rather than "the process working directory" — an empty string in the
/// environment is what an unset variable usually turns into, and trusting it would scatter memory into whatever
/// directory the agent happened to start in.
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
        memory_usage: runtime.join("memory-usage"),
        memory_maintenance: runtime.join("memory-maintenance"),
        runtime,
        root,
    }
}

/// Resolve `path` against `base` unless it is already absolute.
pub fn resolve_from(base: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    }
}

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
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect()
    }

    #[test]
    fn identity_paths_split_repo_from_runtime() {
        let paths = build_identity_paths(Path::new("/mem"), "agent-1");
        assert_eq!(paths.root, Path::new("/mem/agents/agent-1"));
        assert_eq!(paths.repo, Path::new("/mem/agents/agent-1/repo"));
        assert_eq!(paths.runtime, Path::new("/mem/agents/agent-1/runtime"));
    }

    #[test]
    fn every_runtime_subdirectory_is_under_runtime() {
        let paths = build_identity_paths(Path::new("/mem"), "agent-1");
        let all = [
            &paths.locks,
            &paths.transcripts,
            &paths.reflection,
            &paths.reflection_sessions,
            &paths.worktrees,
            &paths.viewers,
            &paths.push_queue,
            &paths.facts_queue,
            &paths.facts,
            &paths.notices,
            &paths.tool_receipts,
            &paths.recall,
            &paths.memory_usage,
            &paths.memory_maintenance,
        ];
        for path in all {
            assert!(
                path.starts_with(&paths.runtime),
                "{} escapes runtime",
                path.display()
            );
        }
        assert_eq!(RUNTIME_SUBDIRNAMES.len(), 14);
    }

    #[test]
    fn recall_ledger_and_pending_live_under_recall() {
        let paths = build_identity_paths(Path::new("/mem"), "agent-1");
        assert!(paths.recall_ledger.starts_with(&paths.recall));
        assert!(paths.recall_pending.starts_with(&paths.recall));
    }

    #[test]
    fn no_content_path_is_inside_runtime() {
        let paths = build_identity_paths(Path::new("/mem"), "agent-1");
        assert!(!paths.repo.starts_with(&paths.runtime));
        assert!(!paths.runtime.starts_with(&paths.repo));
    }

    #[test]
    fn a_set_override_beats_the_default() {
        let root = resolve_memory_root(
            &env(&[(MEMORY_ROOT_ENV_VAR, "/custom")]),
            Path::new("/work"),
        );
        assert_eq!(root, Path::new("/custom"));
    }

    #[test]
    fn a_relative_override_resolves_against_cwd() {
        let root = resolve_memory_root(&env(&[(MEMORY_ROOT_ENV_VAR, "mem")]), Path::new("/work"));
        assert_eq!(root, Path::new("/work/mem"));
    }

    #[test]
    fn a_blank_override_falls_back_to_the_default() {
        let root = resolve_memory_root(&env(&[(MEMORY_ROOT_ENV_VAR, "   ")]), Path::new("/work"));
        assert_eq!(root, default_memory_root());
    }

    #[test]
    fn an_absent_override_falls_back_to_the_default() {
        assert_eq!(
            resolve_memory_root(&env(&[]), Path::new("/work")),
            default_memory_root()
        );
    }

    #[test]
    fn the_default_root_is_under_dot_omo() {
        assert!(default_memory_root().ends_with(".omo/memory"));
    }

    #[test]
    fn resolve_from_keeps_absolute_paths() {
        assert_eq!(
            resolve_from(Path::new("/base"), Path::new("/elsewhere/x")),
            Path::new("/elsewhere/x")
        );
    }

    #[test]
    fn resolve_from_joins_relative_paths() {
        assert_eq!(
            resolve_from(Path::new("/base"), Path::new("a/b")),
            Path::new("/base/a/b")
        );
    }
}
