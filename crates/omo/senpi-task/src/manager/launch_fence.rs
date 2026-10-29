//! Launch run fencing (`manager/launch-fence.ts`): a launch belongs to one run of a record, identified
//! by its run epoch and owning host, so a child whose start resolved after the run moved on is never
//! attached over a newer owner.

use crate::state::{TaskRecord, TaskStatus};

/// `LaunchRun`: the run a launch was started for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchRun {
    pub task_id: String,
    pub epoch: i64,
    pub owner: Option<i64>,
}

/// `launchRunOf`: the run identity of the record a launch started from.
pub fn launch_run_of(record: &TaskRecord) -> LaunchRun {
    LaunchRun {
        task_id: record.task_id.clone(),
        epoch: record.notification.run_epoch,
        owner: record.host_pid,
    }
}

fn is_same_run(run: &LaunchRun, fresh: &TaskRecord) -> bool {
    fresh.notification.run_epoch == run.epoch && fresh.host_pid == run.owner
}

/// `ownsRun`: whether the launch's run still holds the record (running, same epoch, same owner).
pub fn owns_run(run: &LaunchRun, fresh: Option<&TaskRecord>) -> bool {
    fresh.is_some_and(|fresh| fresh.status == TaskStatus::Running && is_same_run(run, fresh))
}
