use std::path::Path;
use std::sync::Arc;

use super::open_file_holders::{HolderLookupOptions, find_open_file_holder_pids};
use super::repo_in_use::find_processes_working_in;

pub type GitLockScanFn = Arc<dyn Fn(&Path) -> Option<Vec<u32>> + Send + Sync>;

#[derive(Clone)]
pub struct GitLockScans {
    pub find_holders: GitLockScanFn,
    pub find_repo_users: GitLockScanFn,
}

impl GitLockScans {
    pub fn production() -> Self {
        Self {
            find_holders: Arc::new(|lock_path| {
                find_open_file_holder_pids(lock_path, HolderLookupOptions::default())
            }),
            find_repo_users: Arc::new(|repo_dir| {
                find_processes_working_in(repo_dir, HolderLookupOptions::default())
            }),
        }
    }
}

impl Default for GitLockScans {
    fn default() -> Self {
        Self::production()
    }
}
