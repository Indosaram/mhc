//! Commands always target the current routing handle (`host-command-dispatch.ts`).
use crate::host::HostError;
use serde_json::Value;
pub trait HostRpcClient: Send + Sync {
    fn wait_for_outcome(&self) -> Option<crate::runners::RunnerOutcome> {
        None
    }
    fn transport_lost(&self, _message: String) {}
    fn get_available_models(
        &self,
    ) -> Result<Vec<crate::runners::pinned_model_equivalence::CatalogModelIdentity>, HostError>
    {
        Err(HostError {
            message: "host model catalog unsupported".into(),
        })
    }
    fn command(&self, command: Value) -> Result<(), HostError>;
    fn get_state(&self) -> Result<Value, HostError> {
        Err(HostError {
            message: "host get_state unsupported".into(),
        })
    }
    fn on_event(
        &self,
        listener: crate::manager::child_handle::ManagedChildListener,
    ) -> crate::manager::child_handle::Unsubscribe;
    fn close(&self, _routing_id: &str) -> Result<(), HostError> {
        Err(HostError {
            message: "host close unsupported".into(),
        })
    }
    fn detach(&self, _routing_id: &str) -> Result<(), HostError> {
        Err(HostError {
            message: "host detach unsupported".into(),
        })
    }
}
pub fn dispatch_host_command(
    client: &dyn HostRpcClient,
    routing_id: Option<&str>,
    mut command: Value,
) -> Result<(), HostError> {
    match command["type"].as_str() {
        Some("prompt" | "steer" | "followUp" | "abort") => client.command(command),
        Some("set_session_context") => {
            if let Some(id) = routing_id {
                command["sessionId"] = serde_json::json!(id);
            }
            client.command(command)
        }
        _ => Err(HostError {
            message: format!("unhandled host session command: {command}"),
        }),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    struct Client(std::sync::Mutex<Option<Value>>);
    impl HostRpcClient for Client {
        fn on_event(
            &self,
            _: crate::manager::child_handle::ManagedChildListener,
        ) -> crate::manager::child_handle::Unsubscribe {
            Box::new(|| {})
        }
        fn command(&self, value: Value) -> Result<(), HostError> {
            *self.0.lock().unwrap() = Some(value);
            Ok(())
        }
    }
    #[test]
    fn context_patch_carries_current_routing_id() {
        let client = Client(Default::default());
        dispatch_host_command(
            &client,
            Some("new-route"),
            serde_json::json!({"type":"set_session_context","patch":{"task_status":"completed"}}),
        )
        .unwrap();
        assert_eq!(
            client.0.lock().unwrap().as_ref().unwrap()["sessionId"],
            "new-route"
        );
    }
}
