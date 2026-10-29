use std::path::Path;

use crate::git::{GitMemoryRepo, InitializeGitRepoOptions};

pub(crate) fn init_test_repo(dir: &Path) -> GitMemoryRepo {
    let repo = GitMemoryRepo::open(dir, "test-agent").expect("open repo");
    repo.init(InitializeGitRepoOptions::default())
        .expect("init repo");
    repo
}
