//! Persistent task record store (`store/` in TypeScript). The on-disk layout and JSON shape match
//! what the TypeScript store writes: `<state>/tasks/<id>.json`, `<state>/logs/<id>.jsonl`.

mod parent_model_fallback_parse;
mod claim;
mod event_log;
mod record_lock;
mod record_parse;
mod record_store;
pub mod rollback_migrate;
pub mod rollback_r0_contract;
mod types;

pub use claim::{ClaimError, ClaimOptions, NameBinding, TaskRecordSaver, claim_task_record};
pub(crate) use event_log::event_log_path;
pub use event_log::redact_event_payload;
pub use record_lock::with_task_record_lock;
pub(crate) use record_lock::with_task_record_lock_options;
pub use record_parse::parse_task_record;
pub use record_store::{TaskRecordStore, TaskRecordStoreResidentFigures, to_storable_task_record};
pub use types::{
    ListTaskRecordsResult, PersistedTaskEvent, StateDirConfig, StoreError, TaskRecordDiagnostic,
    TombstoneResult, resolve_state_dir,
};

#[cfg(test)]
#[path = "store_tests.rs"]
mod tests;

#[cfg(test)]
mod parity_tests;
