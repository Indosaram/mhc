//! senpi `modes/rpc/compact-recovery-checkpoint-types.ts`: a frozen cut and the lease
//! operations used by baseline/recovery consumers.

use crate::compact_recovery_types::CheckpointToken;
use futures::future::BoxFuture;

/// A frozen cut and the lease operations used by baseline/recovery consumers.
pub trait CheckpointOps: Send + Sync {
    fn token(&self) -> &CheckpointToken;
    fn root_key(&self) -> usize;
    fn hold<'a>(
        &'a self,
        read: BoxFuture<'a, Result<(), String>>,
    ) -> BoxFuture<'a, Result<(), String>>;
    fn retain(&self) -> RetainedRoot;
}

/// A retained consumer outlives token retirement until its own end receipt.
pub struct RetainedRoot {
    pub root_key: usize,
    end_fn: Option<Box<dyn FnOnce() + Send + Sync>>,
}

impl RetainedRoot {
    pub fn new(root_key: usize, end_fn: impl FnOnce() + Send + Sync + 'static) -> Self {
        Self {
            root_key,
            end_fn: Some(Box::new(end_fn)),
        }
    }

    pub fn end(&mut self) {
        if let Some(cb) = self.end_fn.take() {
            cb();
        }
    }
}

impl Drop for RetainedRoot {
    fn drop(&mut self) {
        self.end();
    }
}
