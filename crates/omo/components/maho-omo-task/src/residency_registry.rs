use senpi_task::{
    host::HostError,
    lifecycle::{ResidencyRegistry, ResidentHandle, ResidentKind},
    manager::{ManagedChildHandle, TaskManager},
};
use std::sync::Arc;

pub struct ManagerResidencyRegistry {
    pub get_manager: Arc<dyn Fn() -> TaskManager + Send + Sync>,
}
struct ManagedResident(Arc<dyn ManagedChildHandle>);
impl ResidentHandle for ManagedResident {
    fn task_id(&self) -> &str {
        self.0.task_id()
    }
    fn is_same_managed(&self, handle: &Arc<dyn ManagedChildHandle>) -> Option<bool> {
        Some(Arc::ptr_eq(&self.0, handle))
    }
    fn kind(&self) -> ResidentKind {
        if self.0.pid().is_some() || (self.0.has_terminate() && !self.0.is_host_session()) {
            ResidentKind::Rpc
        } else {
            ResidentKind::InProcess
        }
    }
    fn mark_stopping(&self) {
        self.0.mark_stopping();
    }
    fn is_host_session(&self) -> bool {
        self.0.is_host_session()
    }
    fn publish_task_status(&self, status: senpi_task::state::TaskStatus) -> Result<(), HostError> {
        self.0.publish_task_status(status)
    }
    fn pid(&self) -> Option<i64> {
        self.0.pid()
    }
    fn abort(&self) -> Result<(), HostError> {
        self.0.abort()
    }
    fn dispose(&self) -> Result<(), HostError> {
        self.0.dispose()
    }
    fn terminate(&self) -> Result<(), HostError> {
        if self.kind() == ResidentKind::InProcess {
            return Ok(());
        }
        if !self.0.has_terminate() {
            return Err(HostError {
                message: format!("rpc resident {} has no terminate port", self.0.task_id()),
            });
        }
        self.0.terminate()
    }
}
impl ResidencyRegistry for ManagerResidencyRegistry {
    fn get(&self, task_id: &str) -> Option<Arc<dyn ResidentHandle>> {
        (self.get_manager)()
            .get_resident_handle(task_id)
            .map(|handle| Arc::new(ManagedResident(handle)) as Arc<dyn ResidentHandle>)
    }
    fn entries(&self) -> Vec<Arc<dyn ResidentHandle>> {
        (self.get_manager)()
            .resident_task_ids()
            .iter()
            .filter_map(|id| self.get(id))
            .collect()
    }
    fn forget(&self, task_id: &str) {
        (self.get_manager)().forget(task_id);
    }
    fn has_pending_sends(&self, task_id: &str) -> bool {
        (self.get_manager)().has_pending_sends(task_id)
    }
    fn is_launching(&self, task_id: &str) -> bool {
        (self.get_manager)().is_launching(task_id)
    }
    fn try_begin_send(&self, task_id: &str) -> bool {
        (self.get_manager)().try_begin_send(task_id)
    }
    fn end_send(&self, task_id: &str) {
        (self.get_manager)().end_send(task_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use senpi_task::{
        manager::child_handle::{ManagedChildListener, Unsubscribe},
        runners::RunnerOutcome,
    };

    struct FakeHandle {
        pid: Option<i64>,
        has_terminate: bool,
        host_session: bool,
    }

    impl ManagedChildHandle for FakeHandle {
        fn task_id(&self) -> &str {
            "task"
        }
        fn session_id(&self) -> Option<String> {
            None
        }
        fn pid(&self) -> Option<i64> {
            self.pid
        }
        fn steer(&self, _: &str) -> Result<(), HostError> {
            Ok(())
        }
        fn follow_up(&self, _: &str) -> Result<(), HostError> {
            Ok(())
        }
        fn abort(&self) -> Result<(), HostError> {
            Ok(())
        }
        fn subscribe(&self, _: ManagedChildListener) -> Unsubscribe {
            Box::new(|| {})
        }
        fn wait_for_outcome(&self) -> RunnerOutcome {
            RunnerOutcome::Cancelled
        }
        fn last_assistant_text(&self) -> Option<String> {
            None
        }
        fn has_terminate(&self) -> bool {
            self.has_terminate
        }
        fn is_host_session(&self) -> bool {
            self.host_session
        }
        fn terminate(&self) -> Result<(), HostError> {
            Ok(())
        }
        fn dispose(&self) -> Result<(), HostError> {
            Ok(())
        }
    }

    #[test]
    fn pidless_cloud_handle_uses_rpc_teardown_without_reclassifying_host_sessions() {
        for (pid, has_terminate, host_session, expected) in [
            (None, true, false, ResidentKind::Rpc),
            (None, true, true, ResidentKind::InProcess),
            (None, false, false, ResidentKind::InProcess),
            (Some(123), true, false, ResidentKind::Rpc),
        ] {
            let handle = ManagedResident(Arc::new(FakeHandle {
                pid,
                has_terminate,
                host_session,
            }));
            assert_eq!(handle.kind(), expected);
        }
    }
}
