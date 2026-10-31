use std::cell::RefCell;
use std::fs;
use std::path::Path;
use std::process::Command;

use crate::git::GitMemoryRepo;
use crate::git::repo_types::GitCommitAuthor;

use crate::tools::memory::{MemoryToolLock, MemoryToolResult};
use crate::tools::memory_apply_patch::{
    MemoryApplyPatchParams, MemoryApplyPatchResult, run_memory_apply_patch,
};
use crate::tools::tool_errors::MemoryToolError;

pub fn test_author() -> GitCommitAuthor {
    GitCommitAuthor {
        agent_id: "patch-agent".to_string(),
        author_name: "Patch Agent".to_string(),
        author_email: Some("patch@example.test".to_string()),
    }
}

pub fn create_patch_fixture(seed_files: &[(&str, &str)]) -> (tempfile::TempDir, GitMemoryRepo) {
    let temp_dir = tempfile::tempdir().expect("tempdir");
    let author = test_author();
    let repo = GitMemoryRepo::open(temp_dir.path(), &author.agent_id).expect("repo");
    repo.init(None).expect("init");

    let mut paths = Vec::new();
    for (rel_path, content) in seed_files {
        let full_path = temp_dir.path().join(rel_path);
        if let Some(parent) = full_path.parent() {
            fs::create_dir_all(parent).expect("mkdir");
        }
        fs::write(&full_path, content).expect("write");
        paths.push(*rel_path);
    }

    if !paths.is_empty() {
        repo.commit_write(&paths, "seed", &author)
            .expect("commit seed");
    }

    (temp_dir, repo)
}

pub fn test_params(reason: &str, input: &str) -> MemoryApplyPatchParams {
    MemoryApplyPatchParams {
        reason: reason.to_string(),
        input: input.to_string(),
        author: test_author(),
        provenance: None,
    }
}

pub fn memory_apply_patch(
    repo: &GitMemoryRepo,
    params: &MemoryApplyPatchParams,
) -> Result<MemoryApplyPatchResult, MemoryToolError> {
    run_memory_apply_patch(repo, params, None)
}

/// Seed a fixture with raw bytes (non-UTF-8 content the `&str` seeds cannot carry).
pub fn seed_bytes(repo: &GitMemoryRepo, rel_path: &str, bytes: &[u8]) {
    fs::write(repo.dir.join(rel_path), bytes).expect("write bytes");
    repo.commit_write(&[rel_path], "seed bytes", &test_author())
        .expect("commit bytes");
}

/// Run git in `dir`, returning trimmed stdout; panics with stderr on a non-zero exit.
pub fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("spawn git");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout)
        .trim_end()
        .to_string()
}

/// True when `message` is `<prefix><7 lowercase hex><suffix>` exactly.
pub fn matches_short_sha_message(message: &str, prefix: &str, suffix: &str) -> bool {
    message
        .strip_prefix(prefix)
        .and_then(|rest| rest.strip_suffix(suffix))
        .is_some_and(|sha| {
            sha.len() == 7
                && sha
                    .chars()
                    .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
        })
}

/// Lock that records every domain it is asked for and runs the operation inline.
#[derive(Default)]
pub struct RecordingLock {
    pub domains: RefCell<Vec<String>>,
}

impl MemoryToolLock for RecordingLock {
    fn with_lock(
        &self,
        domain: &str,
        operation: &mut dyn FnMut() -> Result<MemoryToolResult, MemoryToolError>,
    ) -> Result<MemoryToolResult, MemoryToolError> {
        self.domains.borrow_mut().push(domain.to_string());
        operation()
    }
}
