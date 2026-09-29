//! Rust port of `@oh-my-opencode/boulder-state`: work-plan ("boulder") state
//! persistence for `<worktree-root>/.omo/boulder.json` (schema version 2).
//!
//! The document is the one the TypeScript writer produces: a root *mirror* of the
//! currently active work, a `works` map keyed by work id, and per-work task sessions.
//! Behaviour and the on-disk bytes are ported 1:1; `parity.md` maps every TypeScript
//! test file to the Rust tests covering it and lists the deliberate divergences (chiefly:
//! a corrupt state file is a typed error here instead of a silent `null`).

mod constants;
mod error;
mod js;
mod plan_checklist;
mod records;
mod shared;
mod storage;
mod time;
mod top_level_task;
mod types;

pub use crate::constants::{
    BOULDER_DIR, BOULDER_FILE, BOULDER_STATE_PATH, NOTEPAD_BASE_PATH, NOTEPAD_DIR,
    PROMETHEUS_PLANS_DIR,
};
pub use crate::error::BoulderStateError;
pub use crate::plan_checklist::{get_plan_checklist, parse_plan_checklist};
pub use crate::records::{BoulderState, BoulderWorkState, TaskSessionState};
pub use crate::shared::{normalize_session_id, normalize_session_id_with_platform};
pub use crate::storage::{
    add_boulder_work, append_session_id, append_session_id_for_work, clear_boulder_state,
    complete_boulder, create_boulder_state, end_task_timer, find_prometheus_plans,
    generate_work_id, get_active_works, get_boulder_file_path, get_boulder_works, get_plan_name,
    get_plan_progress, get_task_session_state, get_work_by_id, get_work_by_plan_name,
    get_work_for_session, get_work_resume_options, read_boulder_state, resolve_boulder_plan_path,
    resolve_boulder_plan_path_for_work, select_active_work, start_task_timer,
    upsert_task_session_state, upsert_task_session_state_for_work, write_boulder_state,
};
pub use crate::top_level_task::read_current_top_level_task;
pub use crate::types::{
    BoulderSessionOrigin, BoulderTaskStatus, BoulderWorkInput, BoulderWorkResumeOption,
    BoulderWorkStatus, CompleteBoulderInput, EndTaskTimerInput, PlanChecklist, PlanProgress,
    SessionPlatform, TaskSessionInput, TaskTimerInput, TopLevelTaskRef, TopLevelTaskSection,
    WorkLookupOptions, WorkOwner,
};
