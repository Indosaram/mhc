use std::future::Future;
use std::pin::Pin;

use crate::state::TaskRecord;
use crate::store::TaskRecordStore;
use super::types::{WorkpoolAgent, WorkpoolCaller, WorkpoolEvent, WorkpoolItem, WorkpoolRecord, WorkpoolSpec, WorkpoolWorker};
use tokio::sync::watch;

pub trait WorkpoolRequest: Send + Sync {
    fn pool(&self) -> &WorkpoolRecord;
    fn item(&self) -> &WorkpoolItem;
    fn worker(&self) -> Option<&WorkpoolWorker>;
    fn current(&self) -> bool;
    fn authorize(&self) -> Result<(), WorkpoolAdmissionError>;
    fn bind(&mut self, task_id: &str, epoch: u32) -> bool;
    fn event(&self, event: WorkpoolEvent);
}
pub trait WorkpoolAdmission: Send + Sync {
    fn tasks(&self) -> &TaskRecordStore;
    fn bind_pool_kernel_tools(&self, pool_id: &str, tools: Vec<crate::runners::in_process::shared_tool_filter::ChildToolRef>) -> Result<(), WorkpoolAdmissionError>;
    fn unbind_pool_kernel_tools(&self, pool_id: &str);
    fn resolve(&self, caller: &WorkpoolCaller, agent: &WorkpoolAgent) -> Result<WorkpoolSpec, WorkpoolAdmissionError>;
    fn authorize_spawn(&self, agent: &WorkpoolAgent, parent_session_id: &str) -> Result<(), WorkpoolAdmissionError>;
    fn has_free_slot(&self, model: &str) -> bool;
    fn request(&self, input: Box<dyn WorkpoolRequest>) -> Box<dyn FnOnce() + Send>;
    fn drain(&self);
    fn get(&self, task_id: &str) -> Option<TaskRecord>;
    fn task_run_epoch(&self, task_id: &str) -> Option<u32>;
    fn pending(&self, task_id: &str) -> bool;
    fn cancel<'a>(&'a self, task_id: &'a str) -> Pin<Box<dyn Future<Output = Result<(), WorkpoolAdmissionError>> + Send + 'a>>;
    fn wait_for<'a>(&'a self, task_id: &'a str, cancel: &'a mut watch::Receiver<bool>) -> Pin<Box<dyn Future<Output = Result<TaskRecord, WorkpoolAdmissionError>> + Send + 'a>>;
}
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("workpool admission failed: {0}")]
pub struct WorkpoolAdmissionError(pub String);
