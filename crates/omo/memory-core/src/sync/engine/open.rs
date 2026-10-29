//! The production engine for one memory repository on this device. A device runs one engine per
//! repository: the lease `<repo>/.git/omo-sync-engine.lock` is taken without waiting (another
//! process on this device already syncing answers `None`), and a crashed holder is reclaimed by the
//! lock layer's dead-owner proof. Applies and renders run under the shared `memory-write` lock, so
//! memory tool writes and sync commits never interleave. The account session (transport, verifier,
//! sealer) is supplied by the harness once the device is signed in and holds its keys.

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;

use super::adapters::device_item_store::{DeviceItemStoreOptions, create_device_item_store};
use super::adapters::git_sync_repo::{GitSyncRepoOptions, create_git_sync_repo};
use super::engine::{EngineStatus, SyncEngine};
use super::engine_deps::EngineDeps;
use super::error::{EngineError, EngineResult};
use super::ports::{Cancel, Clock, Sealer, SyncRepo, Transport, Verifier, WriterLock};
use crate::fs::resilient;
use crate::locks::{
    AcquireLockError, AcquireLockOptions, CreateLockRecordOptions, LockRecord, WithLockError,
    acquire_lock, create_lock_record, memory_writer_lock_path, release_lock, with_lock,
};

/// How long an apply waits for a memory tool write to finish before the cycle is retried.
const WRITER_WAIT_MS: u64 = 30_000;

#[derive(Clone)]
pub struct SyncSession {
    pub transport: Rc<dyn Transport>,
    pub verifier: Rc<dyn Verifier>,
    pub sealer: Rc<dyn Sealer>,
}

pub struct OpenDeviceSyncOptions {
    pub on_status_change: Option<Rc<dyn Fn()>>,
    pub repo_dir: PathBuf,
    /// The device's private state (item peer, capture receipts), outside the repository.
    pub state_dir: PathBuf,
    /// The identity's locks directory, shared with the memory tools.
    pub locks_dir: PathBuf,
    pub session: SyncSession,
    pub machine_id: Option<String>,
    pub log: Option<super::engine_deps::SyncLog>,
}

pub struct DeviceSync {
    pub engine: SyncEngine,
    lease_path: PathBuf,
    lease: LockRecord,
    closed: Cell<bool>,
}

impl DeviceSync {
    pub fn status(&self) -> EngineResult<EngineStatus> {
        self.engine.status()
    }

    pub fn notify_local_commit(&self) {
        self.engine.notify_local_commit();
    }

    /// Stops the engine and releases the lease; idempotent.
    pub async fn close(&self) -> EngineResult<()> {
        if self.closed.replace(true) {
            return Ok(());
        }
        self.engine.stop().await;
        release_lock(&self.lease_path, &self.lease)?;
        Ok(())
    }
}

struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> i64 {
        crate::support::time::now_millis()
    }

    fn after(&self, ms: u64, f: Box<dyn FnOnce()>) -> Cancel {
        let handle = tokio::task::spawn_local(async move {
            tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
            f();
        });
        Box::new(move || handle.abort())
    }
}

struct MemoryWriter {
    locks_dir: PathBuf,
}

impl WriterLock for MemoryWriter {
    fn with_lock(&self, f: &mut dyn FnMut() -> EngineResult<()>) -> EngineResult<()> {
        let record = create_lock_record("memory sync apply", CreateLockRecordOptions::default())
            .map_err(|e| EngineError::Lock(e.to_string()))?;
        let options = AcquireLockOptions {
            wait_timeout_ms: Some(WRITER_WAIT_MS),
            ..AcquireLockOptions::new()
        };
        match with_lock(
            &memory_writer_lock_path(&self.locks_dir),
            &record,
            &options,
            f,
        ) {
            Ok(()) => Ok(()),
            Err(WithLockError::User(error)) => Err(error),
            Err(WithLockError::Acquire(error)) => Err(EngineError::Lock(error.to_string())),
        }
    }
}

/// Opens the engine for `repo_dir`; `None` when another process on this device already holds the lease.
pub fn open_device_sync(options: OpenDeviceSyncOptions) -> EngineResult<Option<DeviceSync>> {
    let repo = create_git_sync_repo(GitSyncRepoOptions {
        root: options.repo_dir.clone(),
        exec: None,
    });
    let sync_dir = repo.git_dir().join("omo-sync");
    resilient::create_dir_all(&sync_dir)?;
    // Outside omo-sync/: retiring a copied journal renames that directory, which must not move the lease.
    let lease_path = repo.git_dir().join("omo-sync-engine.lock");
    let lease = create_lock_record("memory sync engine", CreateLockRecordOptions::default())
        .map_err(|e| EngineError::Lock(e.to_string()))?;
    let wait_none = AcquireLockOptions {
        wait_timeout_ms: Some(0),
        ..AcquireLockOptions::new()
    };
    match acquire_lock(&lease_path, &lease, &wait_none) {
        Ok(()) => {}
        Err(AcquireLockError::Contention(_)) => return Ok(None),
        Err(error) => return Err(EngineError::Lock(error.to_string())),
    }
    let built = (|| -> EngineResult<SyncEngine> {
        resilient::create_dir_all(&options.locks_dir)?;
        let store = create_device_item_store(DeviceItemStoreOptions {
            state_dir: options.state_dir.clone(),
            memory_root: options.repo_dir.clone(),
            sync_dir: sync_dir.clone(),
            machine_id: options.machine_id.clone(),
        });
        Ok(SyncEngine::new(EngineDeps {
            on_status_change: options.on_status_change,
            repo: Rc::new(repo),
            store: Rc::new(store),
            transport: options.session.transport.clone(),
            verifier: options.session.verifier.clone(),
            sealer: options.session.sealer.clone(),
            lock: Rc::new(MemoryWriter {
                locks_dir: options.locks_dir.clone(),
            }),
            clock: Rc::new(SystemClock),
            fault: None,
            log: options.log.clone(),
            batch_bytes: None,
            random: None,
        }))
    })();
    match built {
        Ok(engine) => Ok(Some(DeviceSync {
            engine,
            lease_path,
            lease,
            closed: Cell::new(false),
        })),
        Err(error) => {
            let _ = release_lock(&lease_path, &lease);
            Err(error)
        }
    }
}

#[cfg(test)]
#[path = "open_tests.rs"]
mod tests;
