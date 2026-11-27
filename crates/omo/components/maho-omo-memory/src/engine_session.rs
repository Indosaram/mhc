use memory_core::{
    git::{
        GitMemoryRepo,
        errors::GitError,
        repo_types::{GitCommitAuthor, GitSeedFile, InitializeGitRepoOptions},
    },
    identity::layout::MemoryIdentityPaths,
    locks::{
        AcquireLockOptions, CreateLockRecordOptions, WithLockError, create_lock_record,
        memory_writer_lock_path, with_lock,
    },
    tools::{
        memory::{MemoryToolLock, MemoryToolResult},
        tool_errors::MemoryToolError,
    },
};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Condvar, Mutex, OnceLock},
};

#[derive(Default)]
struct RepoCreationWait {
    outcome: Mutex<(Option<Result<(), String>>, usize)>,
    signal: Condvar,
}

static REPO_CREATIONS: OnceLock<Mutex<HashMap<PathBuf, Arc<RepoCreationWait>>>> = OnceLock::new();

fn repo_creations() -> &'static Mutex<HashMap<PathBuf, Arc<RepoCreationWait>>> {
    REPO_CREATIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

struct CreationGuard {
    key: PathBuf,
    wait: Arc<RepoCreationWait>,
    finished: bool,
}

impl CreationGuard {
    fn finish(mut self, result: Result<(), String>) {
        let mut creations = repo_creations()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        creations.remove(&self.key);
        drop(creations);
        let mut guard = self
            .wait
            .outcome
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.0 = Some(result);
        self.wait.signal.notify_all();
        self.finished = true;
    }
}

impl Drop for CreationGuard {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        let mut creations = repo_creations()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        creations.remove(&self.key);
        drop(creations);
        let mut guard = self
            .wait
            .outcome
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if guard.0.is_none() {
            guard.0 = Some(Err("memory repository creation aborted".to_owned()));
            self.wait.signal.notify_all();
        }
    }
}

fn ensure_repo_initialized_coalesced(
    repo_dir: &Path,
    create: impl FnOnce() -> Result<(), MemoryToolError>,
) -> Result<(), MemoryToolError> {
    enum Role {
        Join(Arc<RepoCreationWait>),
        Lead(CreationGuard),
        AlreadyExists,
    }
    let role = {
        let mut creations = repo_creations()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(wait) = creations.get(repo_dir).cloned() {
            Role::Join(wait)
        } else if repo_dir.join(".git").exists() {
            Role::AlreadyExists
        } else {
            let wait = Arc::new(RepoCreationWait::default());
            creations.insert(repo_dir.to_path_buf(), Arc::clone(&wait));
            Role::Lead(CreationGuard {
                key: repo_dir.to_path_buf(),
                wait,
                finished: false,
            })
        }
    };
    match role {
        Role::AlreadyExists => Ok(()),
        Role::Join(wait) => {
            let mut guard = wait
                .outcome
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            guard.1 += 1;
            wait.signal.notify_all();
            while guard.0.is_none() {
                guard = wait
                    .signal
                    .wait(guard)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            }
            match guard.0.as_ref() {
                Some(Ok(())) => Ok(()),
                Some(Err(err)) => Err(MemoryToolError::new(err.clone())),
                None => Ok(()),
            }
        }
        Role::Lead(guard) => match create() {
            Ok(()) => {
                guard.finish(Ok(()));
                Ok(())
            }
            Err(err) => {
                let message = err.to_string();
                guard.finish(Err(message));
                Err(err)
            }
        },
    }
}
pub struct MemoryEngineSession {
    pub repo: GitMemoryRepo,
    pub lock: MemoryWriterLock,
    pub author: GitCommitAuthor,
}
#[derive(Default, Clone)]
pub struct MemoryEngineSessionOptions {
    pub lock_wait_timeout_ms: Option<u64>,
    pub lock_retry_delay_ms: Option<u64>,
    /// The engine-owned staged-memory hook checker the new repository's hooks run (`memoryHookChecker`).
    pub memory_hook_checker: Option<memory_core::memfs::MemoryHookChecker>,
}
pub struct MemoryWriterLock {
    identity: String,
    path: PathBuf,
    wait_timeout_ms: u64,
    retry_delay_ms: Option<u64>,
}
impl MemoryWriterLock {
    pub fn run<T, E: std::fmt::Display>(
        &self,
        domain: &str,
        operation: impl FnOnce() -> Result<T, E>,
    ) -> Result<T, MemoryToolError> {
        if domain != "memory-write" {
            return Err(MemoryToolError::new(format!(
                "unsupported lock domain '{domain}'"
            )));
        }
        let record = create_lock_record(
            &format!("memory tool ({})", self.identity),
            CreateLockRecordOptions::default(),
        )
        .map_err(|error| MemoryToolError::new(error.to_string()))?;
        with_lock(
            &self.path,
            &record,
            &AcquireLockOptions {
                wait_timeout_ms: Some(self.wait_timeout_ms),
                retry_delay_ms: self.retry_delay_ms,
                ..Default::default()
            },
            operation,
        )
        .map_err(|error| match error {
            WithLockError::User(error) => MemoryToolError::new(error.to_string()),
            WithLockError::Acquire(error) => MemoryToolError::new(error.to_string()),
        })
    }
}
impl MemoryToolLock for MemoryWriterLock {
    fn with_lock(
        &self,
        domain: &str,
        operation: &mut dyn FnMut() -> Result<MemoryToolResult, MemoryToolError>,
    ) -> Result<MemoryToolResult, MemoryToolError> {
        self.run(domain, operation)
    }
}
pub fn prepare_memory_engine_session(
    identity: &str,
    paths: &MemoryIdentityPaths,
    options: MemoryEngineSessionOptions,
) -> Result<MemoryEngineSession, MemoryToolError> {
    crate::context::ensure_identity_runtime_dirs(paths)
        .map_err(|error| MemoryToolError::new(error.to_string()))?;
    let repo = GitMemoryRepo::open(&paths.repo, identity)
        .map_err(|error| MemoryToolError::new(error.to_string()))?;
    let lock = MemoryWriterLock {
        identity: identity.into(),
        path: memory_writer_lock_path(&paths.locks),
        wait_timeout_ms: options.lock_wait_timeout_ms.unwrap_or(5000),
        retry_delay_ms: options.lock_retry_delay_ms,
    };
    ensure_repo_initialized_coalesced(&paths.repo, || {
        lock.run("memory-write", || {
            if !paths.repo.join(".git").exists() {
                repo.init(InitializeGitRepoOptions {
                    seed_files: memory_core::seeds::seeds::build_default_seed_files()
                        .into_iter()
                        .map(|seed| GitSeedFile {
                            relative_path: seed.relative_path,
                            content: seed.content,
                        })
                        .collect(),
                    install_hooks: Some(Arc::new(|dir| {
                        crate::worker::spawn_supervisor_support::install_memory_hooks(dir)
                            .map(|_| ())
                            .map_err(GitError::Io)
                    })),
                    ..Default::default()
                })?;
            }
            Ok::<_, GitError>(())
        })
    })?;
    crate::worker::spawn_supervisor_support::install_memory_hooks(&paths.repo)
        .map_err(|error| MemoryToolError::new(error.to_string()))?;
    Ok(MemoryEngineSession {
        repo,
        lock,
        author: GitCommitAuthor {
            agent_id: identity.into(),
            author_name: identity.into(),
            author_email: None,
        },
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn first_write_seeds_real_repo_and_preserves_existing_head() {
        let dir = tempfile::tempdir().unwrap();
        let paths = memory_core::identity::layout::build_identity_paths(dir.path(), "agent");
        assert!(!paths.repo.exists());
        let session = prepare_memory_engine_session("agent", &paths, Default::default()).unwrap();
        assert!(paths.repo.join("system/persona.md").exists());
        assert!(paths.repo.join("system/human.md").exists());
        assert!(paths.repo.join(".git/hooks/pre-commit").exists());
        let head = session.repo.head().unwrap().unwrap();
        let second = prepare_memory_engine_session("agent", &paths, Default::default()).unwrap();
        assert_eq!(second.repo.head().unwrap(), Some(head));
        assert!(!memory_writer_lock_path(&paths.locks).exists());
        assert_eq!(session.author.agent_id, "agent");
    }
    #[test]
    fn unsupported_domain_does_not_run_mutation() {
        let dir = tempfile::tempdir().unwrap();
        let lock = MemoryWriterLock {
            identity: "agent".into(),
            path: dir.path().join("writer.lock"),
            wait_timeout_ms: 0,
            retry_delay_ms: None,
        };
        assert!(
            lock.run::<(), MemoryToolError>("other", || panic!("operation"))
                .is_err()
        );
        assert!(!lock.path.exists());
    }
    #[test]
    fn mutation_error_releases_lock() {
        let dir = tempfile::tempdir().unwrap();
        let lock = MemoryWriterLock {
            identity: "agent".into(),
            path: dir.path().join("writer.lock"),
            wait_timeout_ms: 0,
            retry_delay_ms: None,
        };
        assert_eq!(
            lock.run::<(), _>("memory-write", || Err("operation failed"))
                .unwrap_err()
                .message,
            "operation failed"
        );
        assert!(!lock.path.exists());
    }

    #[test]
    fn concurrent_repo_creations_coalesce_and_retry_after_failure() {
        let dir = tempfile::tempdir().unwrap();
        let repo_dir = dir.path().join("coalesce-repo");
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let release = Arc::new((Mutex::new(false), Condvar::new()));

        let c1 = Arc::clone(&calls);
        let r1 = Arc::clone(&release);
        let p1 = repo_dir.clone();
        let lead = std::thread::spawn(move || {
            ensure_repo_initialized_coalesced(&p1, || {
                c1.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                entered_tx.send(()).unwrap();
                let mut guard = r1.0.lock().unwrap();
                while !*guard {
                    guard = r1.1.wait(guard).unwrap();
                }
                Err(MemoryToolError::new("transient init failure"))
            })
        });
        entered_rx.recv().unwrap();

        let c2 = Arc::clone(&calls);
        let p2 = repo_dir.clone();
        let joiner = std::thread::spawn(move || {
            ensure_repo_initialized_coalesced(&p2, || {
                c2.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(())
            })
        });
        let wait = {
            let creations = repo_creations().lock().unwrap();
            Arc::clone(creations.get(&repo_dir).expect("inflight creation entry"))
        };
        {
            let mut guard = wait.outcome.lock().unwrap();
            while guard.1 < 1 {
                guard = wait.signal.wait(guard).unwrap();
            }
        }
        {
            let mut guard = release.0.lock().unwrap();
            *guard = true;
            release.1.notify_all();
        }
        assert!(lead.join().unwrap().is_err());
        assert!(joiner.join().unwrap().is_err());
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);

        // Next caller after a failed creation retries cleanly instead of staying poisoned.
        ensure_repo_initialized_coalesced(&repo_dir, || {
            calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            std::fs::create_dir_all(repo_dir.join(".git")).unwrap();
            Ok(())
        })
        .unwrap();
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
    }
}
