use std::fs;

use crate::git::GitMemoryRepo;
use crate::git::repo_types::GitCommitAuthor;

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
