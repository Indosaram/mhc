use std::sync::Arc;

use senpi_task::{manager::TaskManager, workpool::ports::WorkpoolAdmission};

pub fn create_workpool_admission(manager: Arc<TaskManager>) -> Arc<dyn WorkpoolAdmission + Send + Sync> {
    Arc::new(senpi_task::manager::workpool_admission::ManagerWorkpoolAdmission::new(manager))
}

pub fn from_engine(engine: &crate::engine::TaskEngine) -> Arc<dyn WorkpoolAdmission + Send + Sync> {
    create_workpool_admission(engine.manager.clone())
}

pub fn create_workpool_engine(engine: &crate::engine::TaskEngine) -> senpi_task::workpool::engine::WorkpoolEngine {
    senpi_task::workpool::engine::WorkpoolEngine::new(engine.store.state_dir(), from_engine(engine))
}
