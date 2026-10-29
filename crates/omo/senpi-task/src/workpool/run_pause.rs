//! Durable pool Stop/Resume state, separate from task lifecycle ownership.
use crate::pause::admission_barrier::ResumeCredential;
use crate::state::TaskStatus;
use super::dispatcher::WorkpoolDispatcher;
use super::ports::WorkpoolAdmission;
use super::store::WorkpoolStore;
use super::types::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResumePoolResult { Released, Stale, NotHeld }

pub struct PoolRunPause<'a> {
    pub store: &'a WorkpoolStore,
    pub admission: &'a dyn WorkpoolAdmission,
    pub dispatcher: &'a WorkpoolDispatcher,
}

impl PoolRunPause<'_> {
    fn acknowledge(&self, pool: &WorkpoolRecord) -> Result<(), WorkpoolError> {
        for worker in &pool.workers {
            if self.admission.get(&worker.task_id).is_some_and(|task| task.status == TaskStatus::Running) {
                super::worker_turn::acknowledge_worker_turn_with_entries(self.store, self.admission.tasks(),
                    &(pool.pool_id.clone(), pool.generation, worker.task_id.clone(), worker.run_epoch), None)?;
            }
        }
        Ok(())
    }

    pub fn pause_for_run(&self, caller: &WorkpoolCaller, pool_id: &str, pause_seq: u64) -> Result<(), WorkpoolError> {
        let pool = self.store.owned(caller, pool_id)?;
        if pool.status == WorkpoolStatus::Cancelled || pool.hold.as_ref().map_or(0, |h| h.pause_seq) >= pause_seq { return Ok(()); }
        self.store.mutate(pool_id, |current| {
            if current.status == WorkpoolStatus::Cancelled || current.hold.as_ref().map_or(0, |h| h.pause_seq) >= pause_seq { return current.clone(); }
            let mut next = current.clone();
            next.hold = Some(WorkpoolHold { reason: WorkpoolHoldReason::UserStop, pause_seq });
            for worker in &mut next.workers { if worker.status == WorkpoolWorkerStatus::Busy { worker.status = WorkpoolWorkerStatus::Paused; } }
            next
        })?;
        self.dispatcher.cancel(&pool.pool_id);
        Ok(())
    }

    pub fn resume_for_run(&self, caller: &WorkpoolCaller, pool_id: &str, pause_seq: u64, credential: Option<ResumeCredential>) -> Result<ResumePoolResult, WorkpoolError> {
        let pool = self.store.owned(caller, pool_id)?;
        let Some(hold) = &pool.hold else {
            if credential.is_some() { self.acknowledge(&pool)?; self.dispatcher.attach_with_credential(&pool.pool_id, credential)?; }
            return Ok(ResumePoolResult::NotHeld);
        };
        if hold.pause_seq != pause_seq { return Ok(ResumePoolResult::Stale); }
        let resumed = self.store.mutate(pool_id, |current| {
            if current.hold.as_ref().is_none_or(|h| h.pause_seq != pause_seq) { return current.clone(); }
            let mut next = current.clone(); next.hold = None;
            for worker in &mut next.workers {
                if worker.status != WorkpoolWorkerStatus::Paused { continue; }
                let task = self.admission.get(&worker.task_id);
                if task.as_ref().is_some_and(|t| t.status == TaskStatus::Paused) { continue; }
                if task.as_ref().is_some_and(|t| !matches!(t.status, TaskStatus::Pending | TaskStatus::Running)) {
                    worker.status = WorkpoolWorkerStatus::Idle; worker.completed_turns += 1;
                    worker.idle_since = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis() as u64;
                } else { worker.status = WorkpoolWorkerStatus::Busy; }
            }
            next
        })?;
        if resumed.hold.is_some() { return Ok(ResumePoolResult::Stale); }
        self.acknowledge(&resumed)?;
        self.dispatcher.attach_with_credential(&pool.pool_id, credential)?;
        Ok(ResumePoolResult::Released)
    }
}
