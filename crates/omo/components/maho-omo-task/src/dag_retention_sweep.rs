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

/// A deferred sweep that outlives `schedule_dag_retention_sweep` for as long as the store is live.
///
/// The sweep writes DAG store artifacts, so a caller that owns the store's directory must be able
/// to await it before tearing that directory down: otherwise the detached worker publishes
/// `runs/`, `results/` or `events/` entries into a directory the owner already removed.
pub struct DagRetentionSweep {
    done: Option<std::thread::JoinHandle<()>>,
}

impl DagRetentionSweep {
    fn deferred(sweep: Box<dyn FnOnce() + Send>) -> Self {
        Self {
            done: Some(std::thread::spawn(move || sweep())),
        }
    }

    fn completed() -> Self {
        Self { done: None }
    }

    /// Blocks until the deferred sweep has finished publishing to the store.
    pub fn join(mut self) {
        if let Some(done) = self.done.take() {
            // A panicking sweep already reports through its logger; joining must not double-panic.
            let _ = done.join();
        }
    }
}

impl Drop for DagRetentionSweep {
    fn drop(&mut self) {
        if let Some(done) = self.done.take() {
            let _ = done.join();
        }
    }
}

pub fn schedule_dag_retention_sweep(
    store: Arc<DagFileStore>,
    options: DagRetentionSweepOptions,
) -> DagRetentionSweep {
    let logger = options.logger;
    let sweep = Box::new(move || match store.prune_expired(None) {
        Ok(pruned) if !pruned.is_empty() => logger.info(&format!(
            "DAG retention reclaimed {} expired run(s)",
            pruned.len()
        )),
        Ok(_) => {}
        Err(error) => logger.warn(&format!("DAG retention sweep failed: {error}")),
    });
    match options.schedule {
        Some(schedule) => {
            schedule(sweep);
            DagRetentionSweep::completed()
        }
        None => DagRetentionSweep::deferred(sweep),
    }
}

#[cfg(test)]
#[path = "dag_retention_sweep_tests.rs"]
mod tests;
