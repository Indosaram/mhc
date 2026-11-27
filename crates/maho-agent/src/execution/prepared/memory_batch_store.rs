//! Port of senpi `packages/agent/src/execution/prepared/memory-batch-store.ts`.

use std::sync::Arc;

use crate::durable::store::DurableStoreOptions;
use crate::durable::store_memory::{create_memory_store_medium, open_memory_store};
use crate::types::BatchStoreFactory;

/// A batch ledger that lives only in this process for the batch's duration: the in-process (fused) placement.
pub fn open_memory_batch_store() -> BatchStoreFactory {
    Arc::new(|| {
        let medium = create_memory_store_medium();
        Arc::new(open_memory_store(medium, DurableStoreOptions::default()))
    })
}
