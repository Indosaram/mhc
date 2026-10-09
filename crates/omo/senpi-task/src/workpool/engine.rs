use super::dispatcher::WorkpoolDispatcher;
use super::ports::WorkpoolAdmission;
use super::store::WorkpoolStore;
use super::types::{PoolId, WorkpoolCaller, WorkpoolCreate, WorkpoolError, WorkpoolRecord};

/// Persistent workpool engine state. Admission remains a host-owned port, as
/// in senpi; this module owns pool records and dispatch scheduling.
pub struct WorkpoolEngine<'a> {
    pub store: WorkpoolStore,
    pub admission: &'a dyn WorkpoolAdmission,
    pub dispatcher: WorkpoolDispatcher,
}

impl<'a> WorkpoolEngine<'a> {
    pub fn new(state_dir: impl Into<std::path::PathBuf>, admission: &'a dyn WorkpoolAdmission) -> Self {
        Self { store: WorkpoolStore::new(state_dir), admission, dispatcher: WorkpoolDispatcher::new() }
    }

    pub fn inspect(&self, caller: &WorkpoolCaller, pool_id: &str) -> Result<WorkpoolRecord, WorkpoolError> {
        self.store.owned(caller, pool_id)
    }

    pub fn schedule(&mut self, pool_id: &PoolId) { self.dispatcher.schedule(pool_id.clone()); }

    pub fn create(&mut self, caller: &WorkpoolCaller, input: &WorkpoolCreate) -> Result<WorkpoolRecord, WorkpoolError> {
        let spec = self.admission.resolve(caller, &input.agent).map_err(|error| WorkpoolError::new(super::types::WorkpoolErrorCode::AdmissionRefused, error.0))?;
        let mode = input.mode.unwrap_or(super::default_mode::WORKPOOL_DEFAULT_MODE);
        let pool = self.store.create(caller, input, mode, spec)?;
        self.schedule(&pool.pool_id);
        Ok(pool)
    }

    pub fn cancel(&mut self, caller: &WorkpoolCaller, pool_id: &str) -> Result<WorkpoolRecord, WorkpoolError> {
        let owned = self.inspect(caller, pool_id)?;
        let cancelled = self.store.mutate(pool_id, |pool| {
            if pool.status == super::types::WorkpoolStatus::Cancelled { return pool.clone(); }
            let mut next = pool.clone();
            next.status = super::types::WorkpoolStatus::Cancelled;
            next.generation += 1;
            for item in &mut next.items {
                if matches!(item.status, super::types::WorkpoolItemStatus::Queued | super::types::WorkpoolItemStatus::Assigned) {
                    item.status = super::types::WorkpoolItemStatus::Cancelled;
                    item.error = Some(super::types::WorkpoolYieldError { code: "cancelled".into(), message: "Pool cancelled.".into() });
                }
            }
            next
        })?;
        self.dispatcher.cancel(&owned.pool_id);
        for worker in cancelled.workers { let _ = self.admission.cancel(&worker.task_id); }
        Ok(cancelled)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn engine_module_is_registered() { assert_eq!(super::WorkpoolDispatcher::new().is_disposed(), false); }
}
