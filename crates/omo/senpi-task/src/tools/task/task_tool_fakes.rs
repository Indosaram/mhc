//! Rust port of `tools/task/__fixtures__/task-tool-fakes.ts`: builds real task records through a
//! scripted manager (the Rust `TaskManager` is concrete, so records come from a settled start).

use crate::manager::manager_tests::fakes::{base_spec, default_manager, started, wait_terminal};
use crate::state::TaskRecord;

/// `makeRecord`: a settled record from parent session `parent-1`; callers mutate fields as overrides.
pub(crate) fn make_record() -> TaskRecord {
    let harness = default_manager();
    let task = started(harness.manager.start(&base_spec()));
    harness.in_process.wait_handle(&task.task_id).complete("done");
    wait_terminal(&harness.manager, &task.task_id)
}
