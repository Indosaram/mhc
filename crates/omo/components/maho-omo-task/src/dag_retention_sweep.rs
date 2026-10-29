use std::sync::Arc;

use senpi_task::dag::store::DagFileStore;

pub trait DagRetentionSweepLogger: Send + Sync {
    fn info(&self, message: &str);
    fn warn(&self, message: &str);
}

pub type DagRetentionSweepScheduler = Arc<dyn Fn(Box<dyn FnOnce() + Send>) + Send + Sync>;

pub struct DagRetentionSweepOptions {
    pub schedule: Option<DagRetentionSweepScheduler>,
    pub logger: Arc<dyn DagRetentionSweepLogger>,
}

fn defer_past_session_start(sweep: Box<dyn FnOnce() + Send>) {
    std::thread::spawn(sweep);
}

pub fn schedule_dag_retention_sweep(store: Arc<DagFileStore>, options: DagRetentionSweepOptions) {
    let logger = options.logger;
    let sweep = Box::new(move || match store.prune_expired(None) {
        Ok(pruned) if !pruned.is_empty() => logger.info(&format!("DAG retention reclaimed {} expired run(s)", pruned.len())),
        Ok(_) => {},
        Err(error) => logger.warn(&format!("DAG retention sweep failed: {error}")),
    });
    match options.schedule {
        Some(schedule) => schedule(sweep),
        None => defer_past_session_start(sweep),
    }
}

#[cfg(test)]
#[path = "dag_retention_sweep_tests.rs"]
mod tests;
