use std::future::Future;
use std::pin::Pin;

use crate::state::TaskRecord;
use crate::store::TaskRecordStore;
use super::types::{WorkpoolAgent, WorkpoolCaller, WorkpoolEvent, WorkpoolItem, WorkpoolRecord, WorkpoolSpec, WorkpoolWorker};

pub trait WorkpoolRequest {
    fn pool(&self) -> &WorkpoolRecord;
    fn item(&self) -> &WorkpoolItem;
    fn worker(&self) -> Option<&WorkpoolWorker>;
    fn current(&self) -> bool;
    fn authorize(&self) -> Result<(), WorkpoolAdmissionError>;
    fn bind(&mut self, task_id: &str, epoch: u32) -> bool;
    fn event(&self, event: WorkpoolEvent);
}
pub trait WorkpoolAdmission {
    fn tasks(&self) -> &TaskRecordStore;
    fn resolve(&self, caller: &WorkpoolCaller, agent: &WorkpoolAgent) -> Result<WorkpoolSpec, WorkpoolAdmissionError>;
    fn has_free_slot(&self, model: &str) -> bool;
    fn request(&self, input: Box<dyn WorkpoolRequest>) -> Box<dyn FnOnce()>;
    fn drain(&self);
    fn get(&self, task_id: &str) -> Option<TaskRecord>;
    fn pending(&self, task_id: &str) -> bool;
    fn cancel<'a>(&'a self, task_id: &'a str) -> Pin<Box<dyn Future<Output = Result<(), WorkpoolAdmissionError>> + Send + 'a>>;
    fn wait_for<'a>(&'a self, task_id: &'a str) -> Pin<Box<dyn Future<Output = Result<TaskRecord, WorkpoolAdmissionError>> + Send + 'a>>;
}
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("workpool admission failed: {0}")]
pub struct WorkpoolAdmissionError(pub String);
