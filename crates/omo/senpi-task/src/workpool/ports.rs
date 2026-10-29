use std::future::Future;
use std::pin::Pin;

use crate::manager::start_admission::HeldStart;
use crate::pause::admission_barrier::ResumeCredential;
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
    /// The resume credential a resumed turn presents to the admission barrier.
    fn resume_credential(&self) -> Option<ResumeCredential> { None }
}

/// What a dispatched request answers with (`workpool/ports.ts` `request`): a held request dispatched
/// nothing, so its cancel closure only drops the request.
pub struct WorkpoolRequestHandle {
    /// Set when the run is paused: the item's turn stays held until the run resumes.
    pub held: Option<HeldStart>,
    pub cancel: Box<dyn FnOnce() + Send>,
}

impl WorkpoolRequestHandle {
    pub fn dispatched(cancel: Box<dyn FnOnce() + Send>) -> Self {
        Self { held: None, cancel }
    }

    /// A request the admission barrier held: nothing was dispatched and there is nothing to release.
    pub fn held(held: HeldStart) -> Self {
        Self { held: Some(held), cancel: Box::new(|| {}) }
    }

    pub fn is_held(&self) -> bool { self.held.is_some() }
}
pub trait WorkpoolAdmission: Send + Sync {
    fn tasks(&self) -> &TaskRecordStore;
    fn bind_pool_kernel_tools(&self, pool_id: &str, tools: Vec<crate::runners::in_process::shared_tool_filter::ChildToolRef>) -> Result<(), WorkpoolAdmissionError>;
    fn unbind_pool_kernel_tools(&self, pool_id: &str);
    fn resolve(&self, caller: &WorkpoolCaller, agent: &WorkpoolAgent) -> Result<WorkpoolSpec, WorkpoolAdmissionError>;
    fn authorize_spawn(&self, agent: &WorkpoolAgent, parent_session_id: &str) -> Result<(), WorkpoolAdmissionError>;
    fn has_free_slot(&self, model: &str) -> bool;
    fn request(&self, input: Box<dyn WorkpoolRequest>) -> WorkpoolRequestHandle;
    /// Whether the root's admission barrier is closed or resuming: deliveries wait for Resume.
    fn delivery_held(&self, _root_session_id: &str) -> bool { false }
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
