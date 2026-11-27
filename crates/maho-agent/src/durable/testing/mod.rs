//! Port of senpi `packages/agent/src/durable/testing/`.

pub mod kill_store;
pub mod kill_wake;
pub mod manual_timers;
pub mod multi_host_cases;
pub mod store_cases;
pub mod store_harness;
pub mod sync_commit_cases;
pub mod wake_cases;
pub mod wake_harness;
pub mod wake_test_loop;

pub use kill_store::{
    KILL_STORE_CASES, KILL_STORE_DECLARATIONS, KillDeclaration, kill_store_cases,
    kill_store_declarations,
};
pub use kill_wake::{
    KILL_WAKE_CASES, KILL_WAKE_DECLARATIONS, KillWakeDeclaration, KillWakeOperation,
    kill_wake_cases, kill_wake_declarations,
};
pub use manual_timers::{ManualTimers, create_manual_timers};
pub use multi_host_cases::{MULTI_HOST_CASES, multi_host_cases};
pub use store_cases::{STORE_CASES, append, claimed_store, put, store_cases, stored};
pub use store_harness::{
    StoreCaseAdapters, StoreConformanceAdapter, adopt_log, cut_once_at, expect_cut, expect_equal,
    expect_store_error, is_cut_signal, reopen_snapshot,
};
pub use sync_commit_cases::{
    SYNC_COMMIT_CASES, SYNC_COMMIT_REGISTRY, SyncCommitCaseAdapters, SyncCommitConformanceAdapter,
    sync_commit_cases, sync_commit_registry,
};
pub use wake_cases::{WAKE_CASES, wake_cases};
pub use wake_harness::{
    BootOptions, HEARTBEAT_MS, SessionInstance, SessionSlot, WakeAlarmSession, WakeCaseAdapters,
    WakeConformanceAdapter, create_slot, cut_alarm, cut_store, cut_wake, expect_wake_cut,
    restart_host,
};
pub use wake_test_loop::{TestLoop, open_test_loop};
