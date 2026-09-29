//! Git lock contention detection, retry policies, and serialized config mutations.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread::sleep;
use std::time::Duration;

use super::errors::GitError;

const ATTEMPTS: usize = 5;
const BASE_DELAY_MS: u64 = 25;

static CONFIG_LOCKS: Mutex<Option<HashMap<PathBuf, Arc<Mutex<()>>>>> = Mutex::new(None);

fn get_dir_lock(dir: &Path) -> Arc<Mutex<()>> {
    let mut guard = CONFIG_LOCKS.lock().unwrap();
    let map = guard.get_or_insert_with(HashMap::new);
    let key = dir.to_path_buf();
    map.entry(key)
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
}

/// Tests whether an error message or GitError represents Git config file lock contention.
pub fn is_git_config_lock_error_str(text: &str) -> bool {
    let lower = text.to_lowercase();
    lower.contains("could not lock config file")
        || (lower.contains("unable to create '") && lower.contains("config.lock'"))
}

/// Tests whether a GitError represents Git config file lock contention.
pub fn is_git_config_lock_error(err: &GitError) -> bool {
    is_git_config_lock_error_str(&err.to_string())
}

/// Tests whether an error message or GitError represents any transient Git lock contention.
pub fn is_git_lock_error_str(text: &str) -> bool {
    let lower = text.to_lowercase();
    lower.contains("could not lock config file")
        || (lower.contains("unable to create '") && lower.contains(".lock'"))
        || lower.contains("cannot lock ref")
        || lower.contains("another git process seems to be running")
}

/// Tests whether a GitError represents any transient Git lock contention.
pub fn is_git_lock_error(err: &GitError) -> bool {
    is_git_lock_error_str(&err.to_string())
}

/// Executes a config mutation serialized per repository directory with retries.
pub fn with_serialized_git_config_mutation<T, F>(dir: &Path, mutate: F) -> Result<T, GitError>
where
    F: FnMut() -> Result<T, GitError>,
{
    let lock = get_dir_lock(dir);
    let _guard = lock.lock().unwrap();
    retry_config_mutation(mutate)
}

fn retry_config_mutation<T, F>(mut mutate: F) -> Result<T, GitError>
where
    F: FnMut() -> Result<T, GitError>,
{
    for attempt in 1..=ATTEMPTS {
        match mutate() {
            Ok(val) => return Ok(val),
            Err(err) => {
                if !is_git_config_lock_error(&err) || attempt == ATTEMPTS {
                    return Err(err);
                }
                let backoff = BASE_DELAY_MS * (1 << (attempt - 1));
                sleep(Duration::from_millis(backoff));
            }
        }
    }
    Err(GitError::LockExhausted(
        "unreachable: git lock retry exhausted without result".to_string(),
    ))
}

/// Retries an operation on transient index, ref, or config locks with exponential backoff.
pub fn with_git_lock_retry<T, F>(mut operation: F) -> Result<T, GitError>
where
    F: FnMut() -> Result<T, GitError>,
{
    for attempt in 1..=ATTEMPTS {
        match operation() {
            Ok(val) => return Ok(val),
            Err(err) => {
                if !is_git_lock_error(&err) || attempt == ATTEMPTS {
                    return Err(err);
                }
                let backoff = BASE_DELAY_MS * (1 << (attempt - 1));
                sleep(Duration::from_millis(backoff));
            }
        }
    }
    Err(GitError::LockExhausted(
        "unreachable: git lock retry exhausted without result".to_string(),
    ))
}
