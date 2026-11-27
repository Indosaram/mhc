//! Shared doctor / CLI lock-bundle re-exports (`reflection-lock-bundle.ts`) (#10144, #12517).

pub use crate::identity::layout::build_identity_paths;
pub use crate::retention::cap::{describe_runtime_cap_excess, measure_runtime_cap_evictable_bytes};
pub use crate::retention::host_step::create_retention_host_sweep_step;
pub use crate::retention::log::read_last_retention_log_entry;
pub use crate::retention::past_retention::measure_unreflected_journal_bytes;
pub use crate::retention::policy::DEFAULT_RUNTIME_RETENTION;
pub use crate::retention::size::{
    DEFAULT_RUNTIME_MAX_BYTES, format_runtime_bytes, measure_runtime_size_within_budget,
    runtime_size_standing,
};

pub use super::host_sweep::sweep_host_identities;
pub use super::identity_orphan_sweep::sweep_identity_orphan_worktrees;
pub use super::stale_holder::classify_reflection_lock_holder;
pub use super::stale_lock_reaper::{
    decide_stale_reflection_lock, decide_stranded_run, format_stale_lock_fix_lines,
    reclaim_dead_owner_locks, run_stale_lock_reaper,
};
