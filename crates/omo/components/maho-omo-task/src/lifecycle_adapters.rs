use senpi_task::{
    host::HostError,
    lifecycle::{AdmissionResult, DestroyCause, LifecycleError, TaskLifecycle},
    manager::types::SpawnAdmission,
};
pub struct TaskLifecycleDestruction(pub TaskLifecycle);
impl senpi_task::steering::DestructionPort for TaskLifecycleDestruction {
    fn supports_fenced_eviction(&self) -> bool {
        true
    }
    fn destroy_resident_task(&self, id: &str, cause: DestroyCause) -> Result<(), HostError> {
        self.0
            .destroy_resident_task(id, cause)
            .map_err(|error| HostError {
                message: error.to_string(),
            })
    }
    fn evict_managed_if_current(
        &self,
        id: &str,
        handle: &std::sync::Arc<dyn senpi_task::manager::ManagedChildHandle>,
    ) -> Result<(), HostError> {
        self.0
            .evict_managed_if_current(id, handle)
            .map_err(|error| HostError {
                message: error.to_string(),
            })
    }
    fn host_session_cancel(
        &self,
    ) -> Option<senpi_task::lifecycle::host_session::HostSessionCancelPort> {
        Some(self.0.host_session_cancel())
    }
}
impl senpi_task::team::runtime_types::TeamMemberDestructionPort for TaskLifecycleDestruction {
    fn destroy_resident_task(&self, id: &str, cause: DestroyCause) -> Result<(), String> {
        self.0
            .destroy_resident_task(id, cause)
            .map_err(|error| error.to_string())
    }
}
pub fn admit_lifecycle(
    lifecycle: &TaskLifecycle,
    session: &str,
) -> Result<SpawnAdmission, LifecycleError> {
    lifecycle
        .admit_resident(session)
        .map(|admission| match admission {
            AdmissionResult::Admitted => SpawnAdmission::Admitted,
            AdmissionResult::Evicted { evicted_task_id } => {
                SpawnAdmission::Evicted { evicted_task_id }
            }
            AdmissionResult::Rejected(error) => SpawnAdmission::Rejected {
                message: error.to_string(),
                max_children: Some(senpi_task::manager::types::MaxChildren::Number(
                    error.max_children,
                )),
                residents: Some(
                    error
                        .residents
                        .iter()
                        .map(|resident| senpi_task::manager::types::ResidentSummary {
                            task_id: resident.task_id.clone(),
                            name: resident.name.clone(),
                            status: resident.status.as_str().to_string(),
                        })
                        .collect(),
                ),
            },
        })
}
