//! Port of `goal/goal-file-lock.ts` (senpi 903b8336).
//!
//! Upstream uses `proper-lockfile`: a lock directory whose mtime a live holder refreshes, which a
//! contender reclaims once it is older than the stale window. This module keeps that protocol on a
//! directory beside the goal file, plus an owner token so a holder can tell its lock was reclaimed
//! (proper-lockfile's `onCompromised`).

use crate::{
    errors::GoalError,
    persistence::{goal_file_path, write_goal_file},
    types::{Goal, GoalStoreRef},
};
use maho_core::{
    lockfile_policy::{FILE_STORAGE_LOCK_RETRY_MAX_DELAY_MS, FILE_STORAGE_LOCK_RETRY_MIN_DELAY_MS},
    session_sidecar_store::serialize_by_key,
};
use sha2::{Digest, Sha256};
use std::{
    fs,
    future::Future,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant, SystemTime},
};

/// `GOAL_LOCK_OPTIONS`: `{ realpath: false, stale: 10_000, update: 2_000, retries: 0 }`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GoalLockOptions {
    pub stale_ms: u64,
    pub update_ms: u64,
}

/// One policy for every goal-lock contender. A goal read-modify-write takes milliseconds, so a
/// 10s stale window never mistakes a live holder (mtime refreshed every 2s) for a dead one,
/// while a holder killed mid-mutation is reclaimed within ~10s. The wait budget exceeds the
/// stale window, so waiters ride through a crashed holder instead of failing.
pub const GOAL_LOCK_OPTIONS: GoalLockOptions = GoalLockOptions {
    stale_ms: 10_000,
    update_ms: 2_000,
};
pub const GOAL_LOCK_WAIT_BUDGET_MS: u64 = 15_000;

const OWNER_TOKEN_FILE: &str = "owner-token";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoalStoreBusyError {
    pub path: PathBuf,
    pub waited_ms: u64,
}

impl std::fmt::Display for GoalStoreBusyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Goal store is busy: lock on {} was held for {}ms. Another process may be updating the goal; close unused sessions if contention persists.",
            self.path.display(),
            self.waited_ms
        )
    }
}

impl std::error::Error for GoalStoreBusyError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoalStoreLockCompromisedError {
    pub path: PathBuf,
}

impl std::fmt::Display for GoalStoreLockCompromisedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Goal store lock on {} was compromised: another process reclaimed it. The stale write was rejected to preserve the newer goal state.",
            self.path.display()
        )
    }
}

impl std::error::Error for GoalStoreLockCompromisedError {}

// GoalError (errors.rs) is owned elsewhere and has no lock variants; both errors surface as
// `GoalError::Io` carrying the upstream message, prefixed with the upstream error name.
impl From<GoalStoreBusyError> for GoalError {
    fn from(error: GoalStoreBusyError) -> Self {
        GoalError::Io(format!("GoalStoreBusyError: {error}"))
    }
}

impl From<GoalStoreLockCompromisedError> for GoalError {
    fn from(error: GoalStoreLockCompromisedError) -> Self {
        GoalError::Io(format!("GoalStoreLockCompromisedError: {error}"))
    }
}

/// The lock directory sits beside the goal file under a fixed-length name. proper-lockfile's
/// default `<file>.lock` would overflow NAME_MAX for a goal whose encoded basename is already
/// at the 255-byte component limit, which the store supports.
pub fn goal_lock_file_path(reference: &GoalStoreRef) -> PathBuf {
    let file_path = goal_file_path(reference);
    let name = file_path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let digest = Sha256::digest(name.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    file_path
        .parent()
        .unwrap_or(Path::new(""))
        .join(format!(".goal-lock-{}", &digest[..40]))
}

struct LockState {
    released: AtomicBool,
    compromised: Mutex<Option<String>>,
    heartbeat: Mutex<Option<std::thread::Thread>>,
}

/// Capabilities a mutation receives while it holds the goal lock.
#[derive(Clone)]
pub struct HeldGoalLock {
    reference: GoalStoreRef,
    file_path: PathBuf,
    lock_dir: PathBuf,
    token: String,
    state: Arc<LockState>,
}

impl HeldGoalLock {
    fn compromise(&self, cause: String) {
        if self.state.released.load(Ordering::SeqCst) {
            return;
        }
        self.state
            .compromised
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get_or_insert(cause);
    }

    fn owns_directory(&self) -> bool {
        fs::read_to_string(self.lock_dir.join(OWNER_TOKEN_FILE))
            .is_ok_and(|token| token == self.token)
    }

    /// Fails with GoalStoreLockCompromisedError when the lock was reclaimed; call before any side write.
    pub fn assert_held(&self) -> Result<(), GoalStoreLockCompromisedError> {
        match fs::read_to_string(self.lock_dir.join(OWNER_TOKEN_FILE)) {
            Ok(token) if token == self.token => {}
            Ok(_) => self.compromise("lock ownership changed".into()),
            Err(error) => self.compromise(error.to_string()),
        }
        if self
            .state
            .compromised
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_some()
        {
            return Err(GoalStoreLockCompromisedError {
                path: self.file_path.clone(),
            });
        }
        Ok(())
    }

    /// Writes the goal file only while the lock is still held.
    pub fn write(&self, goal: Option<&Goal>) -> Result<(), GoalError> {
        self.assert_held()?;
        write_goal_file(&self.reference, goal)
    }

    fn start_heartbeat(&self, update_ms: u64) {
        let held = self.clone();
        let handle = std::thread::spawn(move || {
            loop {
                std::thread::park_timeout(Duration::from_millis(update_ms));
                if held.state.released.load(Ordering::SeqCst) || held.assert_held().is_err() {
                    break;
                }
                // The owner token's mtime is the lock's liveness clock (proper-lockfile's `update`).
                let touched = fs::OpenOptions::new()
                    .write(true)
                    .open(held.lock_dir.join(OWNER_TOKEN_FILE))
                    .and_then(|token| token.set_modified(SystemTime::now()));
                if let Err(error) = touched {
                    held.compromise(error.to_string());
                    break;
                }
            }
        });
        *self
            .state
            .heartbeat
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(handle.thread().clone());
    }

    fn release(&self) {
        let owned = self.owns_directory();
        self.state.released.store(true, Ordering::SeqCst);
        if let Some(heartbeat) = self
            .state
            .heartbeat
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        {
            heartbeat.unpark();
        }
        // A compromised or already-removed lock cannot be released; the mutation result stands.
        if owned {
            let _ = fs::remove_dir_all(&self.lock_dir);
        }
    }
}

struct ReleaseOnDrop(HeldGoalLock);

impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        self.0.release();
    }
}

/// Age of the lock: the owner token's mtime, or the directory's while a fresh holder has not
/// written its token yet.
fn lock_age_ms(lock_dir: &Path) -> Option<u64> {
    let modified = fs::metadata(lock_dir.join(OWNER_TOKEN_FILE))
        .or_else(|_| fs::metadata(lock_dir))
        .ok()?
        .modified()
        .ok()?;
    Some(modified.elapsed().map_or(0, |age| age.as_millis() as u64))
}

fn try_lock(lock_dir: &Path, options: GoalLockOptions) -> std::io::Result<bool> {
    match fs::create_dir(lock_dir) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            if lock_age_ms(lock_dir).is_some_and(|age| age > options.stale_ms) {
                let _ = fs::remove_dir_all(lock_dir);
                return match fs::create_dir(lock_dir) {
                    Ok(()) => Ok(true),
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
                    Err(error) => Err(error),
                };
            }
            Ok(false)
        }
        Err(error) => Err(error),
    }
}

async fn acquire_goal_lock(
    reference: &GoalStoreRef,
    options: GoalLockOptions,
    wait_budget_ms: u64,
) -> Result<HeldGoalLock, GoalError> {
    let file_path = goal_file_path(reference);
    let lock_dir = goal_lock_file_path(reference);
    // Locking does not create the goal file: a placeholder would make the legacy migration
    // skip a pending import.
    if let Some(parent) = file_path.parent() {
        fs::create_dir_all(parent).map_err(|error| GoalError::Io(error.to_string()))?;
    }
    let started_at = Instant::now();
    let mut attempt: u32 = 0;
    loop {
        if try_lock(&lock_dir, options).map_err(|error| GoalError::Io(error.to_string()))? {
            let token = uuid::Uuid::new_v4().to_string();
            if let Err(error) = fs::write(lock_dir.join(OWNER_TOKEN_FILE), &token) {
                let _ = fs::remove_dir_all(&lock_dir);
                return Err(GoalError::Io(error.to_string()));
            }
            let held = HeldGoalLock {
                reference: reference.clone(),
                file_path,
                lock_dir,
                token,
                state: Arc::new(LockState {
                    released: AtomicBool::new(false),
                    compromised: Mutex::new(None),
                    heartbeat: Mutex::new(None),
                }),
            };
            held.start_heartbeat(options.update_ms);
            return Ok(held);
        }
        let waited_ms = started_at.elapsed().as_millis() as u64;
        if waited_ms >= wait_budget_ms {
            return Err(GoalStoreBusyError {
                path: file_path,
                waited_ms,
            }
            .into());
        }
        let delay_ms = FILE_STORAGE_LOCK_RETRY_MIN_DELAY_MS
            .saturating_mul(2u64.saturating_pow(attempt))
            .min(FILE_STORAGE_LOCK_RETRY_MAX_DELAY_MS)
            .min(wait_budget_ms - waited_ms)
            .max(1);
        attempt = attempt.saturating_add(1);
        tokio::time::sleep(Duration::from_millis(delay_ms)).await;
    }
}

/// Serializes a goal mutation both within and across processes.
///
/// The in-process promise tail (serializeByKey) runs first, so a process has at most one lock
/// acquisition in flight per goal. The file lock then guards the read-modify-write against
/// other processes, and `held.write` refuses to write once the lock has been reclaimed.
pub async fn with_goal_file_lock<T, F, Fut>(reference: &GoalStoreRef, f: F) -> Result<T, GoalError>
where
    F: FnOnce(HeldGoalLock) -> Fut,
    Fut: Future<Output = Result<T, GoalError>>,
{
    with_goal_file_lock_policy(reference, GOAL_LOCK_OPTIONS, GOAL_LOCK_WAIT_BUDGET_MS, f).await
}

/// `with_goal_file_lock` under an explicit policy (tests shorten the wait budget).
pub async fn with_goal_file_lock_policy<T, F, Fut>(
    reference: &GoalStoreRef,
    options: GoalLockOptions,
    wait_budget_ms: u64,
    f: F,
) -> Result<T, GoalError>
where
    F: FnOnce(HeldGoalLock) -> Fut,
    Fut: Future<Output = Result<T, GoalError>>,
{
    let key = goal_file_path(reference).to_string_lossy().into_owned();
    serialize_by_key(&key, async {
        let held = acquire_goal_lock(reference, options, wait_budget_ms).await?;
        // Released on completion and also when the mutation future is dropped mid-flight.
        let _release = ReleaseOnDrop(held.clone());
        f(held).await
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::persistence::read_goal_file;

    fn reference(dir: &tempfile::TempDir, thread: &str) -> GoalStoreRef {
        GoalStoreRef {
            base_dir: dir.path().join("extensions").join("goal"),
            thread_id: thread.into(),
        }
    }

    fn goal(thread: &str, tokens_used: u64) -> Goal {
        serde_json::from_value(serde_json::json!({
            "id":"g","threadId":thread,"objective":"probe","status":"active",
            "tokensUsed":tokens_used,"timeUsedSeconds":0,"createdAt":1,"updatedAt":1
        }))
        .unwrap()
    }

    fn age(lock_dir: &Path, seconds: u64) {
        fs::OpenOptions::new()
            .write(true)
            .open(lock_dir.join(OWNER_TOKEN_FILE))
            .unwrap()
            .set_modified(SystemTime::now() - Duration::from_secs(seconds))
            .unwrap();
    }

    #[test]
    fn lock_path_is_fixed_length_beside_the_goal_file() {
        let dir = tempfile::tempdir().unwrap();
        let long = reference(&dir, &"x".repeat(250));
        let path = goal_lock_file_path(&long);
        assert_eq!(path.parent(), goal_file_path(&long).parent());
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with(".goal-lock-"));
        assert_eq!(name.len(), ".goal-lock-".len() + 40);
        assert_ne!(path, goal_lock_file_path(&reference(&dir, "other")));
    }

    #[tokio::test]
    async fn locking_does_not_create_the_goal_file_and_releases_the_lock() {
        let dir = tempfile::tempdir().unwrap();
        let reference = reference(&dir, "t");
        let lock_dir = goal_lock_file_path(&reference);
        let seen = with_goal_file_lock(&reference, |_held| async {
            Ok((goal_file_path(&reference).exists(), lock_dir.exists()))
        })
        .await
        .unwrap();
        assert_eq!(seen, (false, true));
        assert!(!lock_dir.exists());
    }

    #[tokio::test]
    async fn rejects_a_writer_visibly_instead_of_overwriting_under_a_live_lock() {
        let dir = tempfile::tempdir().unwrap();
        let reference = reference(&dir, "busy");
        write_goal_file(&reference, Some(&goal("busy", 42))).unwrap();
        fs::create_dir_all(goal_lock_file_path(&reference)).unwrap();
        let error =
            with_goal_file_lock_policy(&reference, GOAL_LOCK_OPTIONS, 150, |held| async move {
                held.write(Some(&goal("busy", 1)))
            })
            .await
            .unwrap_err()
            .to_string();
        assert!(
            error.starts_with("GoalStoreBusyError: Goal store is busy"),
            "{error}"
        );
        assert_eq!(read_goal_file(&reference).unwrap().unwrap().tokens_used, 42);
        assert!(goal_lock_file_path(&reference).exists());
    }

    #[tokio::test]
    async fn recovers_the_lock_of_a_crashed_holder() {
        let dir = tempfile::tempdir().unwrap();
        let reference = reference(&dir, "crashed");
        let lock_dir = goal_lock_file_path(&reference);
        fs::create_dir_all(&lock_dir).unwrap();
        fs::write(lock_dir.join(OWNER_TOKEN_FILE), "dead-holder").unwrap();
        age(&lock_dir, 60);
        with_goal_file_lock(&reference, |held| async move {
            held.write(Some(&goal("crashed", 1)))
        })
        .await
        .unwrap();
        assert_eq!(read_goal_file(&reference).unwrap().unwrap().tokens_used, 1);
        assert!(!lock_dir.exists());
    }

    #[tokio::test]
    async fn reclaimed_lock_rejects_the_stale_write_and_keeps_the_replacement() {
        let dir = tempfile::tempdir().unwrap();
        let reference = reference(&dir, "compromised");
        write_goal_file(&reference, Some(&goal("compromised", 7))).unwrap();
        let lock_dir = goal_lock_file_path(&reference);
        let error = with_goal_file_lock(&reference, |held| {
            let lock_dir = lock_dir.clone();
            async move {
                fs::write(lock_dir.join(OWNER_TOKEN_FILE), "new-owner").unwrap();
                assert!(held.assert_held().is_err());
                held.write(Some(&goal("compromised", 1)))
            }
        })
        .await
        .unwrap_err()
        .to_string();
        assert!(
            error.starts_with("GoalStoreLockCompromisedError: Goal store lock on "),
            "{error}"
        );
        assert_eq!(read_goal_file(&reference).unwrap().unwrap().tokens_used, 7);
        assert_eq!(
            fs::read_to_string(lock_dir.join(OWNER_TOKEN_FILE)).unwrap(),
            "new-owner"
        );
    }

    #[tokio::test]
    async fn lands_all_updates_from_two_concurrent_loops_in_one_process() {
        let dir = tempfile::tempdir().unwrap();
        let reference = reference(&dir, "loops");
        write_goal_file(&reference, Some(&goal("loops", 0))).unwrap();
        let bump = |reference: GoalStoreRef| async move {
            for _ in 0..50 {
                with_goal_file_lock(&reference, |held| {
                    let reference = reference.clone();
                    async move {
                        let mut goal = read_goal_file(&reference)?.expect("goal");
                        tokio::task::yield_now().await;
                        goal.tokens_used += 1;
                        held.write(Some(&goal))
                    }
                })
                .await
                .unwrap();
            }
        };
        tokio::join!(bump(reference.clone()), bump(reference.clone()));
        assert_eq!(
            read_goal_file(&reference).unwrap().unwrap().tokens_used,
            100
        );
    }

    #[test]
    fn options_match_upstream_policy() {
        assert_eq!(
            GOAL_LOCK_OPTIONS,
            GoalLockOptions {
                stale_ms: 10_000,
                update_ms: 2_000
            }
        );
        const {
            assert!(GOAL_LOCK_WAIT_BUDGET_MS > GOAL_LOCK_OPTIONS.stale_ms);
        }
    }
}
