use senpi_task::{host::HostError,lifecycle::{TaskLifecycle,DestroyCause,AdmissionResult,LifecycleError},manager::types::SpawnAdmission};
pub struct TaskLifecycleDestruction(pub TaskLifecycle);
impl senpi_task::steering::DestructionPort for TaskLifecycleDestruction {
    fn destroy_resident_task(&self,id:&str,cause:DestroyCause)->Result<(),HostError> { self.0.destroy_resident_task(id,cause).map_err(|error| HostError { message:error.to_string() }) }
}
impl senpi_task::team::runtime_types::TeamMemberDestructionPort for TaskLifecycleDestruction {
    fn destroy_resident_task(&self,id:&str,cause:DestroyCause)->Result<(),String> { self.0.destroy_resident_task(id,cause).map_err(|error| error.to_string()) }
}
pub fn admit_lifecycle(lifecycle:&TaskLifecycle,session:&str)->Result<SpawnAdmission,LifecycleError> {
    lifecycle.admit_resident(session).map(|admission| match admission {
        AdmissionResult::Admitted=>SpawnAdmission::Admitted,
        AdmissionResult::Evicted { evicted_task_id }=>SpawnAdmission::Evicted { evicted_task_id },
        AdmissionResult::Rejected(error)=>SpawnAdmission::Rejected { message:error.to_string() },
    })
}
