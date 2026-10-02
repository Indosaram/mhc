use std::sync::Arc;
use senpi_task::{host::HostError, lifecycle::{ResidencyRegistry, ResidentHandle, ResidentKind}, manager::{ManagedChildHandle, TaskManager}};

pub struct ManagerResidencyRegistry { pub get_manager: Arc<dyn Fn() -> TaskManager + Send + Sync> }
struct ManagedResident(Arc<dyn ManagedChildHandle>);
impl ResidentHandle for ManagedResident {
    fn task_id(&self) -> &str { self.0.task_id() }
    fn kind(&self) -> ResidentKind { if self.0.pid().is_some() { ResidentKind::Rpc } else { ResidentKind::InProcess } }
    fn pid(&self) -> Option<i64> { self.0.pid() }
    fn abort(&self) -> Result<(), HostError> { self.0.abort() }
    fn dispose(&self) -> Result<(), HostError> { self.0.dispose() }
    fn terminate(&self) -> Result<(), HostError> {
        if self.kind() == ResidentKind::InProcess { return Ok(()) }
        if !self.0.has_terminate() { return Err(HostError { message: format!("rpc resident {} has no terminate port", self.0.task_id()) }) }
        self.0.terminate()
    }
}
impl ResidencyRegistry for ManagerResidencyRegistry {
    fn get(&self, task_id: &str) -> Option<Arc<dyn ResidentHandle>> { (self.get_manager)().get_resident_handle(task_id).map(|handle| Arc::new(ManagedResident(handle)) as Arc<dyn ResidentHandle>) }
    fn entries(&self) -> Vec<Arc<dyn ResidentHandle>> { (self.get_manager)().resident_task_ids().iter().filter_map(|id| self.get(id)).collect() }
    fn forget(&self, task_id: &str) { (self.get_manager)().forget(task_id); }
    fn has_pending_sends(&self, _task_id: &str) -> bool { false }
}
