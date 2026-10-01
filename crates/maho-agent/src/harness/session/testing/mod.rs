//! Port of senpi packages/agent/src/harness/session/testing/.

pub mod gating_storage;
pub mod instrumented_storage;
pub mod storage_decorator;

pub use gating_storage::GatingStorage;
pub use instrumented_storage::InstrumentedStorage;
pub use storage_decorator::{StorageDecorator, commit_discarded};
