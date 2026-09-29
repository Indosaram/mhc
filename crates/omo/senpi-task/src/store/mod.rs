//! Persistent task record store (`store/` in TypeScript). The on-disk layout and JSON shape match
//! what the TypeScript store writes: `<state>/tasks/<id>.json`, `<state>/logs/<id>.jsonl`.

mod claim;
mod event_log;
mod record_lock;
mod record_parse;
mod record_store;
mod types;

pub use claim::{ClaimError, ClaimOptions, NameBinding, TaskRecordSaver, claim_task_record};
pub use event_log::redact_event_payload;
pub use record_lock::with_task_record_lock;
pub use record_parse::parse_task_record;
pub use record_store::TaskRecordStore;
pub use types::{
    ListTaskRecordsResult, PersistedTaskEvent, StateDirConfig, StoreError, TaskRecordDiagnostic,
    TombstoneResult, resolve_state_dir,
};

#[cfg(test)]
#[path = "store_tests.rs"]
mod tests;
