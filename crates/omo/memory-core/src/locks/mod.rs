//! Cross-process locks for memory writes, reflection scheduling, and transcript state.

pub mod acquire;
pub mod domains;
pub mod lock_record;
pub mod process_identity;

pub use acquire::{
    AcquireLockError, AcquireLockOptions, LockContentionError, LockNonce, WithLockError,
    acquire_lock, is_held, release_lock, with_lock,
};
pub use domains::{
    LOCK_DOMAINS, LockDomain, LockDomainError, facts_queue_lock_path, facts_runs_lock_path,
    memory_writer_lock_path, notice_lock_path, reflection_scheduler_lock_path,
    run_finalization_lock_path, skills_usage_lock_path, transcript_state_lock_path,
};
pub use lock_record::{
    CreateLockRecordOptions, LockRecord, LockRecordError, create_lock_record, parse_lock_record,
};
pub use process_identity::{ProcessLiveness, get_pid_liveness, get_process_start_identity};
