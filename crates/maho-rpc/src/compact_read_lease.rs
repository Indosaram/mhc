//! senpi `modes/rpc/compact-read-lease.ts` (senpi#4272): the typed read refusal of compact current
//! state and the lease that invalidates a cut immediately while holding its owner's references until
//! the cut's last in-flight operation unwinds.
//!
//! Rust also keeps here the one error type every compact module returns (`CompactError`): upstream
//! throws its separate error classes through one `Error` channel.
use crate::compact_content_store::CompactWriteError;
use crate::compact_spool_dir::CompactSpoolRefusedError;
use crate::compact_storage_io::CompactStorageReadError;
use crate::compact_transition::CompactTransitionError;
use std::future::Future;
use std::sync::{Mutex, MutexGuard, PoisonError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactReadReason {
    Released,
    Retired,
    Range,
    SizeLimit,
    CountLimit,
    Ownership,
}

impl CompactReadReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Released => "released",
            Self::Retired => "retired",
            Self::Range => "range",
            Self::SizeLimit => "size_limit",
            Self::CountLimit => "count_limit",
            Self::Ownership => "ownership",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("compact read refused ({})", .reason.as_str())]
pub struct CompactReadError {
    pub reason: CompactReadReason,
}

impl CompactReadError {
    pub fn new(reason: CompactReadReason) -> Self {
        Self { reason }
    }
}

/// Every failure a compact module reports; upstream throws these classes through one channel.
#[derive(Debug, Clone, thiserror::Error)]
pub enum CompactError {
    #[error(transparent)]
    Read(#[from] CompactReadError),
    #[error(transparent)]
    Write(#[from] CompactWriteError),
    #[error(transparent)]
    StorageRead(#[from] CompactStorageReadError),
    #[error(transparent)]
    SpoolRefused(#[from] CompactSpoolRefusedError),
    #[error(transparent)]
    Transition(#[from] CompactTransitionError),
    /// A host I/O or codec failure outside the typed refusals (upstream rethrows the raw cause).
    #[error("{0}")]
    Io(String),
}

impl CompactError {
    pub fn read(reason: CompactReadReason) -> Self {
        Self::Read(CompactReadError::new(reason))
    }

    /// The read refusal reason, when this is a `CompactReadError`.
    pub fn read_reason(&self) -> Option<CompactReadReason> {
        match self {
            Self::Read(error) => Some(error.reason),
            _ => None,
        }
    }

    pub fn is_retired(&self) -> bool {
        self.read_reason() == Some(CompactReadReason::Retired)
    }
}

impl From<serde_json::Error> for CompactError {
    fn from(error: serde_json::Error) -> Self {
        Self::Io(error.to_string())
    }
}

/// The compact modules never hold a lock across an await, so a poisoned lock keeps its data.
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

type Validity = Box<dyn Fn() -> Result<(), CompactError> + Send + Sync>;

struct LeaseState {
    released: bool,
    operations: usize,
    dispose: Option<Box<dyn FnOnce() + Send>>,
}

/// Invalidates immediately, but holds the owner's references until its last operation unwinds.
pub struct CompactReadLease {
    state: Mutex<LeaseState>,
    valid: Validity,
}

impl CompactReadLease {
    pub fn new(dispose: impl FnOnce() + Send + 'static) -> Self {
        Self::with_valid(dispose, || Ok(()))
    }

    pub fn with_valid(
        dispose: impl FnOnce() + Send + 'static,
        valid: impl Fn() -> Result<(), CompactError> + Send + Sync + 'static,
    ) -> Self {
        Self {
            state: Mutex::new(LeaseState {
                released: false,
                operations: 0,
                dispose: Some(Box::new(dispose)),
            }),
            valid: Box::new(valid),
        }
    }

    pub fn assert(&self) -> Result<(), CompactError> {
        if lock(&self.state).released {
            return Err(CompactError::read(CompactReadReason::Released));
        }
        (self.valid)()
    }

    pub async fn run<T>(
        &self,
        read: impl Future<Output = Result<T, CompactError>>,
    ) -> Result<T, CompactError> {
        self.assert()?;
        lock(&self.state).operations += 1;
        // Dropping the operation mid-read unwinds it like a settled one.
        let _operation = OperationGuard(self);
        let result = read.await?;
        self.assert()?;
        Ok(result)
    }

    pub fn release(&self) {
        let dispose = {
            let mut state = lock(&self.state);
            if state.released {
                return;
            }
            state.released = true;
            if state.operations == 0 {
                state.dispose.take()
            } else {
                None
            }
        };
        if let Some(dispose) = dispose {
            dispose();
        }
    }

    pub fn released(&self) -> bool {
        lock(&self.state).released
    }
}

struct OperationGuard<'a>(&'a CompactReadLease);

impl Drop for OperationGuard<'_> {
    fn drop(&mut self) {
        let dispose = {
            let mut state = lock(&self.0.state);
            state.operations -= 1;
            if state.operations == 0 && state.released {
                state.dispose.take()
            } else {
                None
            }
        };
        if let Some(dispose) = dispose {
            dispose();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn release_waits_for_the_last_operation_and_refuses_new_ones() {
        let disposed = Arc::new(AtomicUsize::new(0));
        let counter = disposed.clone();
        let lease = CompactReadLease::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        });
        let (enter_tx, enter_rx) = tokio::sync::oneshot::channel::<()>();
        let (gate_tx, gate_rx) = tokio::sync::oneshot::channel::<()>();
        let operation = lease.run(async move {
            let _ = enter_tx.send(());
            let _ = gate_rx.await;
            Ok(1)
        });
        let (result, ()) = tokio::join!(operation, async {
            let _ = enter_rx.await;
            lease.release();
            assert_eq!(disposed.load(Ordering::SeqCst), 0);
            let _ = gate_tx.send(());
        });
        // The operation finished after release: its result is refused, and the owner is disposed once.
        assert_eq!(
            result.err().and_then(|error| error.read_reason()),
            Some(CompactReadReason::Released)
        );
        assert_eq!(disposed.load(Ordering::SeqCst), 1);
        lease.release();
        assert_eq!(disposed.load(Ordering::SeqCst), 1);
        assert!(lease.assert().is_err());
    }

    #[test]
    fn idle_release_disposes_immediately() {
        let disposed = Arc::new(AtomicUsize::new(0));
        let counter = disposed.clone();
        let lease = CompactReadLease::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        });
        lease.release();
        assert_eq!(disposed.load(Ordering::SeqCst), 1);
    }
}
