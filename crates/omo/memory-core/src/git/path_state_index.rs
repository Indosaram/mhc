//! Atomic Git index file updates with cacheinfo and staging locks.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::support::random::random_uuid;

use super::errors::GitError;
use super::exec::{GitExec, GitExecOptions};
use super::path_state::GitIndexIdentity;
use super::repo_arguments::command_error;

const GIT_TIMEOUT_MS: u64 = 30_000;

/// Atomically updates a single index entry if the current state matches expected identity.
pub fn write_index_if_identity<F>(
    dir: &Path,
    exec: &dyn GitExec,
    path: &str,
    expected: Option<&GitIndexIdentity>,
    next: Option<&GitIndexIdentity>,
    capture: F,
) -> Result<bool, GitError>
where
    F: Fn() -> Result<Option<GitIndexIdentity>, GitError>,
{
    let mut env = BTreeMap::new();
    env.insert("GIT_TERMINAL_PROMPT".to_string(), "0".to_string());

    let opts = GitExecOptions {
        cwd: dir.to_path_buf(),
        timeout_ms: GIT_TIMEOUT_MS,
        env: env.clone(),
        stdin: None,
    };

    let argv = vec![
        "rev-parse".to_string(),
        "--git-path".to_string(),
        "index".to_string(),
    ];
    let res = exec.run(&argv, &opts)?;
    if res.code != 0 {
        return Err(command_error(&argv, &res));
    }

    let raw_path = res.stdout.trim();
    let index_path = if Path::new(raw_path).is_absolute() {
        PathBuf::from(raw_path)
    } else {
        dir.join(raw_path)
    };

    let lock_path = PathBuf::from(format!("{}.lock", index_path.display()));
    let temp_name = format!(
        "{}.omo-{}-{}",
        index_path.display(),
        std::process::id(),
        random_uuid()
    );
    let temp_path = PathBuf::from(temp_name);

    let mut lock_opts = OpenOptions::new();
    lock_opts.write(true).create_new(true);

    let lock_file = match lock_opts.open(&lock_path) {
        Ok(f) => f,
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => return Ok(false),
        Err(err) => return Err(GitError::Io(err)),
    };
    drop(lock_file);

    let cleanup_lock = |lock: &Path| {
        let _ = fs::remove_file(lock);
    };

    let current = match capture() {
        Ok(c) => c,
        Err(err) => {
            cleanup_lock(&lock_path);
            return Err(err);
        }
    };

    if current.as_ref() != expected {
        cleanup_lock(&lock_path);
        return Ok(false);
    }

    if let Err(err) = fs::copy(&index_path, &temp_path) {
        cleanup_lock(&lock_path);
        return Err(GitError::Io(err));
    }

    let mut git_index_env = env;
    git_index_env.insert(
        "GIT_INDEX_FILE".to_string(),
        temp_path.to_string_lossy().into_owned(),
    );

    let update_opts = GitExecOptions {
        cwd: dir.to_path_buf(),
        timeout_ms: GIT_TIMEOUT_MS,
        env: git_index_env,
        stdin: None,
    };

    let update_argv = match next {
        None => vec![
            "update-index".to_string(),
            "--force-remove".to_string(),
            "--".to_string(),
            path.to_string(),
        ],
        Some(identity) => vec![
            "update-index".to_string(),
            "--add".to_string(),
            "--cacheinfo".to_string(),
            identity.mode.clone(),
            identity.oid.clone(),
            path.to_string(),
        ],
    };

    let update_res = match exec.run(&update_argv, &update_opts) {
        Ok(r) => r,
        Err(err) => {
            let _ = fs::remove_file(&temp_path);
            cleanup_lock(&lock_path);
            return Err(GitError::Io(err));
        }
    };

    if update_res.code != 0 {
        let _ = fs::remove_file(&temp_path);
        cleanup_lock(&lock_path);
        return Err(command_error(&update_argv, &update_res));
    }

    let publish_res = (|| -> std::io::Result<()> {
        let bytes = fs::read(&temp_path)?;
        let mut f = File::create(&lock_path)?;
        f.write_all(&bytes)?;
        f.sync_all()?;
        fs::rename(&lock_path, &index_path)?;
        let _ = fs::remove_file(&temp_path);
        Ok(())
    })();

    match publish_res {
        Ok(()) => Ok(true),
        Err(err) => {
            let _ = fs::remove_file(&temp_path);
            cleanup_lock(&lock_path);
            Err(GitError::Io(err))
        }
    }
}
