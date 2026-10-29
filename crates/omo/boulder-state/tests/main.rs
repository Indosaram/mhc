//! Single integration-test binary for this crate: each file under tests/ is one module, so
//! the crate links one test executable instead of one per file.

#[path = "lifecycle.rs"]
mod lifecycle;
#[path = "normalize_session_id.rs"]
mod normalize_session_id;
#[path = "on_disk_compat.rs"]
mod on_disk_compat;
#[path = "plan_checklist.rs"]
mod plan_checklist;
#[path = "plan_progress.rs"]
mod plan_progress;
#[path = "read_state.rs"]
mod read_state;
#[path = "stale_work.rs"]
mod stale_work;
