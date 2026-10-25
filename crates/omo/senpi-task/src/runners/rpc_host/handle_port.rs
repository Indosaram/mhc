//! Hosted-session contracts (`rpc-host/handle-port.ts`).
use crate::{
    host::HostError,
    manager::child_handle::{ManagedChildListener, Unsubscribe},
};
use serde_json::Value;
use std::sync::Arc;
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostSessionIdentity {
    pub routing_id: String,
    pub session_path: String,
    pub instance_id: String,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostSessionReplaced {
    pub session_file: String,
    pub durable_session_id: String,
}
pub trait HostSessionPort: Send + Sync {
    fn wait_for_outcome(&self) -> Option<crate::runners::RunnerOutcome> {
        None
    }
    fn transport_lost(&self, _message: String) {}
    fn watch_engine_turn(
        &self,
    ) -> Option<crate::runners::in_process::native_run_pause::EngineTurnWatch> {
        None
    }
    fn consume_turn_claim(&self) -> bool {
        false
    }
    fn native_pause(&self) -> Option<Arc<dyn crate::pause::native_child_pause::NativeRunPause>> {
        None
    }
    fn get_available_models(
        &self,
    ) -> Result<Vec<crate::runners::pinned_model_equivalence::CatalogModelIdentity>, HostError>
    {
        Err(HostError {
            message: "host model catalog unsupported".into(),
        })
    }
    fn send(&self, command: Value) -> Result<(), HostError>;
    fn get_state(&self) -> Result<Value, HostError>;
    fn on_event(&self, listener: ManagedChildListener) -> Unsubscribe;
    fn close(&self) -> Result<(), HostError>;
    fn detach(&self) -> Result<(), HostError>;
    fn task_parent_wire(&self) -> Option<Arc<crate::runners::task_parent_control::TaskParentWire>> {
        None
    }
}
