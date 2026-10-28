pub type OwnedSessionsCallback = Arc<dyn Fn(&str) -> Vec<String> + Send + Sync>;
use senpi_task::{
    pause::{
        admission_barrier::{
            AdmissionBarrier, AdmissionEntry, AdmissionParticipant, ResumeCredential,
        },
        ports::{RunRef, RunRefKind},
    },
    workpool::{
        engine::WorkpoolEngine,
        store::WorkpoolStore,
        types::{WorkpoolCaller, WorkpoolError, WorkpoolErrorCode, WorkpoolRecord, WorkpoolStatus},
    },
};
use std::{collections::BTreeSet, sync::Arc};
pub struct WorkpoolRunPauseSource {
    pub store: WorkpoolStore,
    pub barrier: AdmissionBarrier,
    pub workpools: Arc<tokio::sync::Mutex<WorkpoolEngine>>,
    pub owned_sessions: OwnedSessionsCallback,
}
impl WorkpoolRunPauseSource {
    fn owned(&self, parent: &str) -> Vec<WorkpoolRecord> {
        let sessions: BTreeSet<_> = (self.owned_sessions)(parent).into_iter().collect();
        self.store
            .list()
            .into_iter()
            .filter(|pool| {
                sessions.contains(&pool.parent_session_id)
                    && pool.status != WorkpoolStatus::Cancelled
            })
            .collect()
    }
    pub fn refs(&self, parent: &str) -> Vec<RunRef> {
        self.owned(parent)
            .into_iter()
            .map(|pool| RunRef::new(RunRefKind::Workpool, pool.pool_id))
            .collect()
    }
    pub fn run_ref_for_task(&self, task: &str) -> Option<RunRef> {
        self.store
            .list()
            .into_iter()
            .find(|pool| {
                pool.workers.iter().any(|worker| worker.task_id == task)
                    || pool.items.iter().any(|item| {
                        item.binding
                            .as_ref()
                            .is_some_and(|binding| binding.task_id == task)
                    })
            })
            .map(|pool| RunRef::new(RunRefKind::Workpool, pool.pool_id))
    }
    pub fn quiesce(&self, parent: &str, pause_seq: u64) -> Result<(), WorkpoolError> {
        let engine = self.workpools.try_lock().map_err(|error| {
            WorkpoolError::new(WorkpoolErrorCode::AdmissionRefused, error.to_string())
        })?;
        for pool in self.owned(parent) {
            engine.pause_for_run(
                &WorkpoolCaller {
                    session_id: pool.parent_session_id.clone(),
                    root_session_id: pool.root_session_id.clone(),
                    depth: 0,
                    cwd: String::new(),
                },
                &pool.pool_id,
                pause_seq,
            )?;
        }
        Ok(())
    }
    pub async fn install(
        &self,
        parent: &str,
        pause_seq: u64,
        credential: ResumeCredential,
    ) -> Result<(), WorkpoolError> {
        for pool in self.owned(parent) {
            let entered = self.barrier.enter(
                parent,
                AdmissionParticipant {
                    key: format!("run:workpool:{}", pool.pool_id),
                    run_ref: RunRef::new(RunRefKind::Workpool, &pool.pool_id),
                },
                Some(credential.clone()),
            );
            let AdmissionEntry::Entered { ticket } = entered else {
                return Err(held());
            };
            let guard = TicketGuard(ticket);
            let result = self.workpools.lock().await.resume_for_run(
                &WorkpoolCaller {
                    session_id: pool.parent_session_id.clone(),
                    root_session_id: pool.root_session_id.clone(),
                    depth: 0,
                    cwd: String::new(),
                },
                &pool.pool_id,
                pause_seq,
                Some(credential.clone()),
            );
            drop(guard);
            if result? == senpi_task::workpool::run_pause::ResumePoolResult::Stale {
                return Err(held());
            }
        }
        Ok(())
    }
    pub async fn released(&self, parent: &str) -> Result<(), WorkpoolError> {
        let sessions: BTreeSet<_> = self
            .owned(parent)
            .into_iter()
            .map(|pool| pool.parent_session_id)
            .collect();
        let engine = self.workpools.lock().await;
        for session in sessions {
            engine.attach(&session, None)?;
        }
        Ok(())
    }
    pub async fn cancel(&self, parent: &str, run_ref: &RunRef) -> Result<(), WorkpoolError> {
        if let Some(pool) = self
            .owned(parent)
            .into_iter()
            .find(|pool| run_ref.kind == RunRefKind::Workpool && pool.pool_id == run_ref.id)
        {
            self.workpools
                .lock()
                .await
                .cancel(
                    &WorkpoolCaller {
                        session_id: pool.parent_session_id,
                        root_session_id: pool.root_session_id,
                        depth: 0,
                        cwd: String::new(),
                    },
                    &pool.pool_id,
                )
                .await?;
        }
        Ok(())
    }
}
struct TicketGuard(senpi_task::pause::admission_barrier::AdmissionTicket);
impl Drop for TicketGuard {
    fn drop(&mut self) {
        self.0.leave();
    }
}
/// These sync wrappers run inside `EngineRunPause`'s own `futures::executor::block_on`, and that
/// executor panics when entered twice on one thread, so the inner future runs on a scoped thread
/// that keeps the caller's tokio runtime context for any task the engine spawns.
fn block_on_nested<F>(future: F) -> F::Output
where
    F: std::future::Future + Send,
    F::Output: Send,
{
    let runtime = tokio::runtime::Handle::try_current().ok();
    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                let _context = runtime.as_ref().map(tokio::runtime::Handle::enter);
                futures::executor::block_on(future)
            })
            .join()
            .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
    })
}
fn held() -> WorkpoolError {
    WorkpoolError::new(
        WorkpoolErrorCode::AdmissionRefused,
        "Run pause admission held",
    )
}

impl crate::engine_run_pause::RunPauseSource for WorkpoolRunPauseSource {
    fn refs(&self, parent: &str) -> Vec<RunRef> {
        WorkpoolRunPauseSource::refs(self, parent)
    }
    fn run_ref_for_task(&self, task: &str) -> Option<RunRef> {
        WorkpoolRunPauseSource::run_ref_for_task(self, task)
    }
    fn quiesce(&self, parent: &str, seq: u64) {
        if let Err(error) = WorkpoolRunPauseSource::quiesce(self, parent, seq) {
            maho_ext_api::diagnostic(&format!("workpool Stop failed: {error}"));
        }
    }
    fn install(&self, parent: &str, seq: u64, credential: &ResumeCredential) -> Result<(), String> {
        block_on_nested(WorkpoolRunPauseSource::install(
            self,
            parent,
            seq,
            credential.clone(),
        ))
        .map_err(|error| error.to_string())
    }
    fn released(&self, parent: &str) -> Result<(), String> {
        block_on_nested(WorkpoolRunPauseSource::released(self, parent))
            .map_err(|error| error.to_string())
    }
    fn cancel(&self, parent: &str, run: &RunRef) -> Result<(), String> {
        block_on_nested(WorkpoolRunPauseSource::cancel(self, parent, run))
            .map_err(|error| error.to_string())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn nested_block_on_inside_an_outer_executor_does_not_panic() {
        let value = futures::executor::block_on(async { super::block_on_nested(async { 7 }) });
        assert_eq!(value, 7);
    }
}
