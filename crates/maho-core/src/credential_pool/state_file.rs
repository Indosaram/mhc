//! Port of credential-pool/state-file.ts held-lock acquisition.
use crate::lockfile_policy::{CredentialStoreBusyError, LockGuard, acquire_lock_async};

pub async fn acquire_state_file_lock(path: &str, budget_ms: u64) -> Result<LockGuard, CredentialStoreBusyError> {
    acquire_lock_async(path, budget_ms).await
}
