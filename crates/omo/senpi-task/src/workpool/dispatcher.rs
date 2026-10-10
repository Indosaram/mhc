use super::types::{WorkpoolWorker, WorkpoolWorkerStatus};

/// Returns idle workers in the same stable priority order as senpi.
pub fn order_idle_workers(workers: &[WorkpoolWorker]) -> Vec<WorkpoolWorker> {
    let mut result: Vec<_> = workers.iter().filter(|worker| worker.status == WorkpoolWorkerStatus::Idle).cloned().collect();
    result.sort_by(|left, right| {
        left.completed_turns.cmp(&right.completed_turns)
            .then_with(|| left.idle_since.cmp(&right.idle_since))
            .then_with(|| left.task_id.cmp(&right.task_id))
    });
    result
}

/// Dispatcher state shared by the engine and its admission integration.
/// Scheduling is deliberately represented as durable state transitions: callers
/// invoke `schedule` after a receipt and `take_scheduled` performs the one-shot
/// event-loop handoff owned by the host.
#[derive(Debug, Default)]
pub struct WorkpoolDispatcher {
    scheduled: std::collections::BTreeSet<String>,
    disposed: bool,
}

impl WorkpoolDispatcher {
    pub fn new() -> Self { Self::default() }

    pub fn schedule(&mut self, pool_id: impl Into<String>) {
        if !self.disposed { self.scheduled.insert(pool_id.into()); }
    }

    pub fn take_scheduled(&mut self, pool_id: &str) -> bool {
        self.scheduled.remove(pool_id)
    }

    pub fn cancel(&mut self, pool_id: &str) { self.scheduled.remove(pool_id); }

    pub fn stop_scheduling(&mut self) {
        self.disposed = true;
        self.scheduled.clear();
    }

    pub fn is_disposed(&self) -> bool { self.disposed }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn worker(id: &str, turns: u32, idle_since: u64, status: WorkpoolWorkerStatus) -> WorkpoolWorker {
        WorkpoolWorker { task_id: id.into(), run_epoch: 0, status, completed_turns: turns, idle_since }
    }

    #[test]
    fn idle_workers_follow_turn_idle_time_then_id() {
        let ordered = order_idle_workers(&[
            worker("z", 1, 2, WorkpoolWorkerStatus::Idle),
            worker("a", 0, 9, WorkpoolWorkerStatus::Idle),
            worker("b", 0, 9, WorkpoolWorkerStatus::Idle),
            worker("busy", 0, 0, WorkpoolWorkerStatus::Busy),
        ]);
        assert_eq!(ordered.iter().map(|worker| worker.task_id.as_str()).collect::<Vec<_>>(), ["a", "b", "z"]);
    }

    #[test]
    fn scheduling_is_coalesced_and_stops() {
        let mut dispatcher = WorkpoolDispatcher::new();
        dispatcher.schedule("wp_a");
        dispatcher.schedule("wp_a");
        assert!(dispatcher.take_scheduled("wp_a"));
        assert!(!dispatcher.take_scheduled("wp_a"));
        dispatcher.stop_scheduling();
        dispatcher.schedule("wp_b");
        assert!(!dispatcher.take_scheduled("wp_b"));
    }
}
