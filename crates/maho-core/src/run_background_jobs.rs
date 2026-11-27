//! Port of senpi packages/coding-agent/src/core/run-background-jobs.ts.
//!
//! Tracks background jobs started by each turn so `interrupt { background: "stop" }` can terminate
//! them without letting stale completions wake the session later.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use futures::future::join_all;

pub trait RunBackgroundJob: Send + Sync {
    fn kind(&self) -> &'static str {
        "bash"
    }
    fn id(&self) -> &str;
    fn description(&self) -> Option<&str> {
        None
    }
    fn stop<'a>(
        &'a self,
        turn_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoppedRunBackgroundJob {
    pub kind: &'static str,
    pub id: String,
    pub description: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FailedRunBackgroundJob {
    pub kind: &'static str,
    pub id: String,
    pub description: Option<String>,
    pub reason: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RunBackgroundStopOutcome {
    pub stopped: Vec<StoppedRunBackgroundJob>,
    pub failed: Vec<FailedRunBackgroundJob>,
}

fn named(job: &dyn RunBackgroundJob) -> StoppedRunBackgroundJob {
    StoppedRunBackgroundJob {
        kind: job.kind(),
        id: job.id().to_owned(),
        description: job.description().map(str::to_owned),
    }
}

#[derive(Default)]
pub struct RunBackgroundJobs {
    by_turn: std::sync::Mutex<HashMap<String, Vec<Arc<dyn RunBackgroundJob>>>>,
}

impl RunBackgroundJobs {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(
        self: &Arc<Self>,
        turn_id: impl Into<String>,
        job: Arc<dyn RunBackgroundJob>,
    ) -> Box<dyn FnOnce() + Send> {
        let turn_id = turn_id.into();
        {
            let mut map = self
                .by_turn
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            map.entry(turn_id.clone())
                .or_default()
                .push(Arc::clone(&job));
        }
        let registry = Arc::downgrade(self);
        let job_id = job.id().to_owned();
        let ptr = Arc::as_ptr(&job) as *const () as usize;
        Box::new(move || {
            if let Some(reg) = registry.upgrade() {
                reg.drop_job(&turn_id, &job_id, ptr);
            }
        })
    }

    pub async fn stop(&self, turn_id: &str) -> RunBackgroundStopOutcome {
        let jobs = {
            let mut map = self
                .by_turn
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            map.remove(turn_id).unwrap_or_default()
        };
        let results = join_all(jobs.iter().map(|job| job.stop(turn_id))).await;
        let mut outcome = RunBackgroundStopOutcome::default();
        for (job, res) in jobs.into_iter().zip(results) {
            let info = named(job.as_ref());
            match res {
                Ok(()) => outcome.stopped.push(info),
                Err(reason) => outcome.failed.push(FailedRunBackgroundJob {
                    kind: info.kind,
                    id: info.id,
                    description: info.description,
                    reason,
                }),
            }
        }
        outcome
    }

    fn drop_job(&self, turn_id: &str, job_id: &str, ptr: usize) {
        let mut map = self
            .by_turn
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(jobs) = map.get_mut(turn_id) else {
            return;
        };
        jobs.retain(|j| !(j.id() == job_id && (Arc::as_ptr(j) as *const () as usize) == ptr));
        if jobs.is_empty() {
            map.remove(turn_id);
        }
    }
}
