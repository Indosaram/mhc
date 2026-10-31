//! senpi `modes/rpc/compact-spool-storage.ts`: the spool file path of compact current-state storage
//! (senpi#4272, R3.C6.4) - one serial asynchronous job chain, the private epoch directory, spool file
//! writes, reads and reclamation, and the failure ledger. `CompactContentStore` adds leaf ownership and
//! spilling on top.
//!
//! Upstream chains jobs on one promise; here a single pump task drains the job queue in FIFO order.
//! The pump is spawned on the current tokio runtime when work is queued; `idle()` spawns it too, so a
//! store created outside a runtime still drains once awaited.
use crate::compact_content_store::CompactContentStore;
use crate::compact_read_lease::{CompactError, CompactReadReason};
use crate::compact_spool_dir::{SpoolDirError, create_spool_dir, remove_spool_dir};
use crate::compact_storage_io::{
    CompactFileRange, CompactStorageFailure, CompactStorageOperation, CompactStorageReadError,
};
use futures::future::BoxFuture;
use std::collections::VecDeque;

pub const RECLAIM_WORK_ALLOWANCE_BYTES: i64 = 1024 * 1024;
const RECLAIM_RETRY_DELAYS_MS: [u64; 5] = [100, 400, 1_600, 6_400, 25_600];
const FAILURE_HISTORY: usize = 32;

pub type Job =
    Box<dyn FnOnce(CompactContentStore) -> BoxFuture<'static, Result<(), CompactError>> + Send>;

/// Boxes one storage job (the closure's future is boxed as the job chain's dyn future).
pub fn job<F, Fut>(f: F) -> Job
where
    F: FnOnce(CompactContentStore) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = Result<(), CompactError>> + Send + 'static,
{
    Box::new(move |store| Box::pin(f(store)))
}

#[derive(Default)]
pub(crate) struct SpoolState {
    pub(crate) failures: VecDeque<CompactStorageFailure>,
    pub(crate) next_file: u64,
    pub(crate) spool_token: Option<String>,
    pub(crate) jobs: VecDeque<(CompactStorageOperation, Job)>,
    pub(crate) pump_running: bool,
    pub(crate) queued: usize,
    pub(crate) spill_queued: bool,
    pub(crate) halted: bool,
    pub(crate) retired: bool,
    pub(crate) pending_io_copy_bytes: i64,
    pub(crate) spill_bytes_written: i64,
    pub(crate) reclaim_debt_bytes: i64,
    pub(crate) reads: usize,
}

impl SpoolState {
    pub(crate) fn assert_readable(&self) -> Result<(), CompactError> {
        if self.retired {
            return Err(CompactError::read(CompactReadReason::Retired));
        }
        Ok(())
    }

    pub(crate) fn spill_allowed(&self) -> bool {
        !self.halted && !self.retired
    }

    /// True while a storage failure or unreclaimed debt above the work allowance holds spilling back.
    pub(crate) fn spill_stopped(&self) -> bool {
        self.halted || self.reclaim_debt_bytes > RECLAIM_WORK_ALLOWANCE_BYTES
    }

    pub(crate) fn enqueue(&mut self, operation: CompactStorageOperation, job: Job) {
        self.queued += 1;
        self.jobs.push_back((operation, job));
    }

    pub(crate) fn fail(
        &mut self,
        operation: CompactStorageOperation,
        path: &str,
        cause: &dyn std::fmt::Display,
        retained: i64,
    ) {
        if matches!(
            operation,
            CompactStorageOperation::SpillWrite
                | CompactStorageOperation::ValueWrite
                | CompactStorageOperation::SpoolCreate
        ) {
            self.halted = true;
        }
        self.failures.push_back(CompactStorageFailure {
            operation,
            path: path.to_owned(),
            cause: cause.to_string(),
            retained_in_memory_bytes: usize::try_from(retained.max(0)).unwrap_or(usize::MAX),
        });
        if self.failures.len() > FAILURE_HISTORY {
            self.failures.pop_front();
        }
    }
}

/// Ends one read; retirement waits for every read to end before it removes the directory.
pub struct ReadGuard {
    store: CompactContentStore,
}

impl Drop for ReadGuard {
    fn drop(&mut self) {
        let done = {
            let mut state = self.store.lock();
            state.spool.reads -= 1;
            state.spool.reads == 0
        };
        if done {
            self.store.0.changed.notify_waiters();
        }
    }
}

impl CompactContentStore {
    pub fn assert_readable(&self) -> Result<(), CompactError> {
        self.lock().spool.assert_readable()
    }

    pub fn begin_read(&self) -> Result<ReadGuard, CompactError> {
        let mut state = self.lock();
        state.spool.assert_readable()?;
        state.spool.reads += 1;
        Ok(ReadGuard {
            store: self.clone(),
        })
    }

    pub fn spill_allowed(&self) -> bool {
        self.lock().spool.spill_allowed()
    }

    pub(crate) fn spill_stopped(&self) -> bool {
        self.lock().spool.spill_stopped()
    }

    /// Queues one storage job behind every job queued before it.
    pub fn enqueue(&self, operation: CompactStorageOperation, job: Job) {
        self.with(|state| state.spool.enqueue(operation, job));
    }

    /// Starts the pump when jobs are queued and none is draining them.
    pub(crate) fn start_pump(&self) {
        if tokio::runtime::Handle::try_current().is_err() {
            return;
        }
        let start = {
            let mut state = self.lock();
            let start = !state.spool.pump_running && !state.spool.jobs.is_empty();
            if start {
                state.spool.pump_running = true;
            }
            start
        };
        if start {
            let store = self.clone();
            tokio::spawn(async move { store.pump().await });
        }
    }

    async fn pump(self) {
        loop {
            let next = {
                let mut state = self.lock();
                let next = state.spool.jobs.pop_front();
                if next.is_none() {
                    state.spool.pump_running = false;
                }
                next
            };
            let Some((operation, job)) = next else {
                self.0.changed.notify_waiters();
                return;
            };
            let result = job(self.clone()).await;
            {
                let mut state = self.lock();
                if let Err(cause) = result {
                    if operation == CompactStorageOperation::SpillWrite {
                        state.spool.spill_queued = false;
                    }
                    let dir = self.0.dir.clone();
                    state.spool.fail(operation, &dir, &cause, 0);
                }
                state.spool.queued -= 1;
            }
            self.0.changed.notify_waiters();
        }
    }

    /// Resolves once every queued storage job, including the ones they queued, has completed.
    pub async fn idle(&self) {
        loop {
            let notified = self.0.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.lock().spool.queued == 0 {
                return;
            }
            self.start_pump();
            notified.await;
        }
    }

    /// Invalidates spilling, joins owned I/O, then removes the epoch directory only when this store
    /// created it and it still passes the ownership checks; otherwise fails with `CompactSpoolRefusedError`.
    pub async fn retire(&self) -> Result<(), CompactError> {
        self.lock().spool.retired = true;
        loop {
            let notified = self.0.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.lock().spool.reads == 0 {
                break;
            }
            notified.await;
        }
        self.idle().await;
        let token = self.lock().spool.spool_token.take();
        if let Some(token) = token {
            remove_spool_dir(&self.0.dir, &token)
                .await
                .map_err(|error| match error {
                    SpoolDirError::Refused(refused) => CompactError::SpoolRefused(refused),
                    SpoolDirError::Io(io) => CompactError::Io(io.to_string()),
                })?;
        }
        Ok(())
    }

    /// Reads a committed file, or the `range` of it, and rejects a missing, unreadable or short one.
    pub async fn read_file(
        &self,
        path: &str,
        bytes: usize,
        operation: CompactStorageOperation,
        range: Option<CompactFileRange>,
    ) -> Result<Vec<u8>, CompactError> {
        let _read = self.begin_read()?;
        let data = match self.0.io.read_file(path.to_owned(), range).await {
            Ok(data) => data,
            Err(cause) => return Err(self.read_failure(operation, path, &cause)),
        };
        if data.len() != bytes {
            let cause = format!("expected {bytes} bytes, found {}", data.len());
            return Err(self.read_failure(operation, path, &cause));
        }
        self.assert_readable()?;
        Ok(data)
    }

    /// Writes one new spool file; `None` means the write failed and was reported (a spill write also
    /// halts spilling).
    pub async fn write_file(
        &self,
        data: Vec<u8>,
        operation: CompactStorageOperation,
        retained_in_memory_bytes: i64,
    ) -> Option<String> {
        if !self.ensure_dir(retained_in_memory_bytes).await {
            return None;
        }
        let prefix = match operation {
            CompactStorageOperation::SpillWrite => "leaf",
            CompactStorageOperation::ValueWrite => "value",
            _ => "page",
        };
        let length = i64::try_from(data.len()).unwrap_or(i64::MAX);
        let file = {
            let mut state = self.lock();
            state.spool.next_file += 1;
            state.spool.pending_io_copy_bytes += length;
            std::path::Path::new(&self.0.dir)
                .join(format!("{prefix}-{}.bin", state.spool.next_file))
                .display()
                .to_string()
        };
        let result = self.0.io.write_file(file.clone(), data).await;
        self.with(|state| {
            state.spool.pending_io_copy_bytes -= length;
            match result {
                Ok(()) => {
                    state.spool.spill_bytes_written += length;
                    Some(file)
                }
                Err(cause) => {
                    state
                        .spool
                        .fail(operation, &file, &cause, retained_in_memory_bytes);
                    state.discard(&file, length);
                    None
                }
            }
        })
    }

    /// Creates the private epoch directory once; a refusal or failure halts spilling and is reported.
    async fn ensure_dir(&self, retained: i64) -> bool {
        *self
            .0
            .dir_ready
            .get_or_init(|| async {
                match create_spool_dir(&self.0.dir).await {
                    Ok(token) => {
                        self.lock().spool.spool_token = Some(token);
                        true
                    }
                    Err(cause) => {
                        let dir = self.0.dir.clone();
                        self.lock().spool.fail(
                            CompactStorageOperation::SpoolCreate,
                            &dir,
                            &cause,
                            retained,
                        );
                        false
                    }
                }
            })
            .await
    }

    /// An unreferenced (or partially written) spool file: reclamation debt until its unlink completes
    /// or finds it gone. Files dropped after retirement started belong to the directory retirement removes.
    pub fn discard(&self, path: &str, bytes: i64) {
        self.with(|state| state.discard(path, bytes));
    }

    /// A failed unlink keeps its debt and is retried with bounded backoff outside the job chain.
    pub(crate) async fn reclaim(&self, path: String, bytes: i64, attempt: usize) {
        if let Err(cause) = self.0.io.unlink(path.clone()).await
            && cause.kind() != std::io::ErrorKind::NotFound
        {
            self.lock()
                .spool
                .fail(CompactStorageOperation::Reclaim, &path, &cause, 0);
            if let Some(delay) = RECLAIM_RETRY_DELAYS_MS.get(attempt).copied() {
                let store = self.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
                    store.with(|state| {
                        if state.spool.retired {
                            state.spool.reclaim_debt_bytes -= bytes;
                        } else {
                            state.enqueue_reclaim(path, bytes, attempt + 1);
                        }
                    });
                });
            }
            return;
        }
        self.with(|state| {
            state.spool.reclaim_debt_bytes -= bytes;
            state.schedule_spill();
        });
    }

    fn read_failure(
        &self,
        operation: CompactStorageOperation,
        path: &str,
        cause: &dyn std::fmt::Display,
    ) -> CompactError {
        self.lock().spool.fail(operation, path, cause, 0);
        CompactError::StorageRead(CompactStorageReadError {
            operation,
            path: path.to_owned(),
            cause: cause.to_string(),
        })
    }
}

impl crate::compact_content_store::StoreState {
    pub(crate) fn discard(&mut self, path: &str, bytes: i64) {
        if self.spool.retired {
            return;
        }
        self.spool.reclaim_debt_bytes += bytes;
        self.enqueue_reclaim(path.to_owned(), bytes, 0);
    }

    pub(crate) fn enqueue_reclaim(&mut self, path: String, bytes: i64, attempt: usize) {
        self.spool.enqueue(
            CompactStorageOperation::Reclaim,
            job(move |store| async move {
                store.reclaim(path, bytes, attempt).await;
                Ok(())
            }),
        );
    }
}
