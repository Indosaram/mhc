//! Cross-process locks for memory writes, reflection scheduling, and transcript state.

pub mod acquire;
pub mod candidate_sweep;
pub mod counting_lease;
pub mod domains;
pub mod lock_record;
pub mod os_file_lock;
pub mod process_identity;
pub mod proven_dead_takeovers;
pub mod recall_wake_domain;
pub mod reflection_run_domain;
pub mod reflection_run_lease;

pub use acquire::{
    AcquireLockError, AcquireLockOptions, LockContentionError, LockNonce, WithLockError,
    acquire_lock, delay, is_held, is_lock_owner_proven_dead, release_lock, with_lock,
};
pub use candidate_sweep::{
    CANDIDATE_STALE_AGE_MS, CANDIDATE_UNLINK_ATTEMPTS, CandidateSweepOptions,
    default_is_sharing_error, forget_leaked_candidate, is_leaked_candidate_name,
    sweep_stale_lock_candidates, track_leaked_candidate,
};
pub use counting_lease::{
    CountingLease, CountingLeaseBusyError, CountingLeaseDomain, CountingLeaseError,
    CountingLeaseOptions, acquire_counting_lease, counting_lease_lock_path,
    counting_lease_ticket_directory,
};
pub use domains::{
    LOCK_DOMAINS, LockDomain, LockDomainError, facts_queue_lock_path, facts_runs_lock_path,
    memory_writer_lock_path, notice_lock_path, receipts_lock_path, reflection_scheduler_lock_path,
    run_finalization_lock_path, skills_usage_lock_path, transcript_state_lock_path,
};
pub use lock_record::{
    CreateLockRecordOptions, LockRecord, LockRecordError, create_lock_record, parse_lock_record,
};
pub use os_file_lock::{
    OsLockAttempt, classify_flock_failure, os_file_lock_available, try_lock_exclusive,
    try_lock_shared,
};
pub use process_identity::{
    ProcessLiveness, get_pid_liveness, get_process_start_identity, start_identities_comparable,
    start_identities_conflict,
};
pub use recall_wake_domain::{
    RECALL_WAKE_DEFAULT_SLOTS, RecallWakeBusyError, RecallWakeError, RecallWakeLease,
    RecallWakeLeaseOptions, acquire_recall_wake_lease, recall_wake_lock_path,
    recall_wake_ticket_directory, with_recall_wake_lease,
};
pub use reflection_run_lease::{
    REFLECTION_RUN_DEFAULT_SLOTS, ReflectionRunLease, ReflectionRunLeaseError,
    ReflectionRunLeaseOptions, acquire_reflection_run_lease, reflection_run_lock_path,
    reflection_run_ticket_directory,
};
