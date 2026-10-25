//! Hosted status publication and teardown (`handle-teardown.ts`).
use super::handle_port::HostSessionPort;
use crate::{host::HostError, state::TaskStatus};
use std::sync::{Arc, Mutex};
pub struct HandleTeardown {
    port: Arc<Mutex<Arc<dyn HostSessionPort>>>,
    state: Mutex<(bool, Option<TaskStatus>, bool, u8)>,
}
pub fn create_handle_teardown(
    port: Arc<Mutex<Arc<dyn HostSessionPort>>>,
    opened_attached: bool,
) -> HandleTeardown {
    HandleTeardown {
        port,
        state: Mutex::new((opened_attached, None, false, 0)),
    }
}
impl HandleTeardown {
    pub fn publish_task_status(&self, status: TaskStatus) -> Result<(), HostError> {
        let mut state = self.state.lock().expect("hosted child state lock poisoned");
        if state.2 {
            return Ok(());
        }
        state.0 = status != TaskStatus::Running;
        state.1 = Some(status);
        drop(state);
        let port = self.port.lock().expect("hosted child state lock poisoned").clone();
        port.send(serde_json::json!({"type":"set_session_context","patch":{"task_status":status.as_str()}}))
    }
    pub fn publish_running(&self) -> Result<(), HostError> {
        if !self.state.lock().expect("hosted child state lock poisoned").0 {
            return Ok(());
        }
        self.publish_task_status(TaskStatus::Running)
    }
    pub fn republish_status(&self) -> Result<(), HostError> {
        let status = self.state.lock().expect("hosted child state lock poisoned").1;
        if let Some(status) = status.filter(|status| *status != TaskStatus::Running) {
            self.publish_task_status(status)?;
        }
        Ok(())
    }
    pub fn detach(&self) -> Result<(), HostError> {
        let port = self.port.lock().expect("hosted child state lock poisoned").clone();
        port.detach()
    }
    pub fn close(&self) -> Result<(), HostError> {
        self.end_on_host(1)
    }
    pub fn terminate(&self) -> Result<(), HostError> {
        self.end_on_host(2)
    }
    fn end_on_host(&self, intent: u8) -> Result<(), HostError> {
        let mut state = self.state.lock().expect("hosted child state lock poisoned");
        state.3 = state.3.max(intent);
        if state.2 {
            return Ok(());
        }
        state.2 = true;
        drop(state);
        let port = self.port.lock().expect("hosted child state lock poisoned").clone();
        if intent == 2
            && let Err(error) = port.send(serde_json::json!({"type":"abort"})) {
                eprintln!("host abort failed: {error}");
            }
        port.close()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manager::child_handle::{ManagedChildListener, Unsubscribe};
    struct Port(Mutex<Vec<serde_json::Value>>);
    impl HostSessionPort for Port {
        fn send(&self, command: serde_json::Value) -> Result<(), HostError> {
            self.0.lock().unwrap().push(command);
            Ok(())
        }
        fn get_state(&self) -> Result<serde_json::Value, HostError> {
            Ok(serde_json::json!({}))
        }
        fn on_event(&self, _: ManagedChildListener) -> Unsubscribe {
            Box::new(|| {})
        }
        fn close(&self) -> Result<(), HostError> {
            Ok(())
        }
        fn detach(&self) -> Result<(), HostError> {
            Ok(())
        }
    }
    #[test]
    fn terminal_is_republished_on_recovery_and_running_precedes_next_turn() {
        let port = Arc::new(Port(Mutex::new(vec![])));
        let current: Arc<Mutex<Arc<dyn HostSessionPort>>> = Arc::new(Mutex::new(port.clone()));
        let teardown = create_handle_teardown(current, false);
        teardown.publish_task_status(TaskStatus::Completed).unwrap();
        teardown.republish_status().unwrap();
        teardown.publish_running().unwrap();
        teardown.publish_running().unwrap();
        let sent = port.0.lock().unwrap();
        assert_eq!(sent.len(), 3);
        assert_eq!(sent[0]["patch"]["task_status"], "completed");
        assert_eq!(sent[1]["patch"]["task_status"], "completed");
        assert_eq!(sent[2]["patch"]["task_status"], "running");
    }
}
