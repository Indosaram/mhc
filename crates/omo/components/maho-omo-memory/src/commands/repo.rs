//! Git repo helpers shared by the memory commands.
//! Port of `components/memory/commands/repo.ts` at pin 77f3067f1.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use memory_core::git::{
    GitError, GitExec, GitExecOptions, GitExecResult, GitHookInstaller, GitMemoryRepo,
    GitMemoryRepoOptions, system_git_exec,
};
use memory_core::memfs::install_hooks;

use super::types::{MemoryCommandDeps, MemoryCommandIdentity};

const RAW_GIT_TIMEOUT_MS: u64 = 30_000;

pub fn open_repo(
    deps: &MemoryCommandDeps,
    identity: &MemoryCommandIdentity,
) -> Result<GitMemoryRepo, String> {
    let installer: GitHookInstaller =
        Arc::new(|dir: &Path| install_hooks(dir).map(|_| ()).map_err(GitError::Io));
    let options = GitMemoryRepoOptions {
        dir: identity.identity_paths.repo.clone(),
        agent_id: identity.identity.clone(),
        exec: deps.exec.clone(),
        install_hooks: Some(installer),
    };
    GitMemoryRepo::new(options).map_err(|error| error.to_string())
}

/// Raw git for read-only queries `GitMemoryRepo` does not expose (diff, worktree list).
pub fn run_git(
    deps: &MemoryCommandDeps,
    dir: &Path,
    argv: &[&str],
) -> std::io::Result<GitExecResult> {
    let exec: Arc<dyn GitExec> = deps.exec.clone().unwrap_or_else(system_git_exec);
    let argv: Vec<String> = argv.iter().map(|arg| (*arg).to_owned()).collect();
    let mut env: BTreeMap<String, String> = std::env::vars().collect();
    env.insert("GIT_TERMINAL_PROMPT".to_owned(), "0".to_owned());
    exec.run(
        &argv,
        &GitExecOptions {
            cwd: dir.to_path_buf(),
            timeout_ms: RAW_GIT_TIMEOUT_MS,
            env,
            stdin: None,
        },
    )
}

pub fn short_sha(sha: &str) -> String {
    sha.chars().take(8).collect()
}

/// True only when an initialized git repo exists; shelling out otherwise throws ENOENT.
pub fn has_git_repo(identity: &MemoryCommandIdentity) -> bool {
    repo_git_dir(identity).exists()
}

pub fn repo_git_dir(identity: &MemoryCommandIdentity) -> PathBuf {
    identity.identity_paths.repo.join(".git")
}
